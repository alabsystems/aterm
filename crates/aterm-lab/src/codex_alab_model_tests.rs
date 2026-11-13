// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicI64, Ordering};

use aterm_agent::supervise::policy::needs::{self, needs_you, says_no_needs, without_needs};
use aterm_agent::supervise::policy::turn_end::{
    LAND_CHECK, Said, classify_said, clean_defense, says_done,
};

use super::*;

fn answer(body: &J) -> Answered {
    answerer_with_clock(|| 1_000)(body)
}

/// LAND'S CONTRADICTION IS TAGGED BY ITS WORDS (aterm's slice 4c): aterm's own contradiction,
/// on either leg, tags `+contradicted`; one that offers a way out, in any case, or drops
/// Land's sentence on files no repository holds tags `+contradicted-wrong`; Land's plain nudge
/// tags nothing, so codex-alab's proof judges the contradiction only where its fork typed one.
/// Negative controls: the escapes matched case-sensitively pass "Ignore it."; the
/// no-repository want dropped passes the cut close.
#[test]
fn the_contradiction_is_tagged_by_its_words() {
    use aterm_agent::harness::repo_work::{RepoRead, RepoWork};
    let found = RepoRead::Found(RepoWork {
        repo: "/private/tmp/at.x/proj/land".to_string(),
        untracked: 1,
        paths: vec!["wip-notes.txt".to_string()],
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    });
    let git = turn_end::land_contradicted(&found, None, &[], None).expect("git's leg");
    let list =
        turn_end::land_contradicted(&RepoRead::Clean, Some("The docs PR is open."), &[], None)
            .expect("the list");
    for text in [&git, &list] {
        assert_eq!(contradiction_tag(text), CONTRADICTED_TAG, "{text}");
    }
    for wrong in [
        format!("{git} Ignore it."),
        format!("{git} Or move it out."),
        git.replace(turn_end::NO_REPOSITORY, ""),
        format!("{} the docs are fine.", turn_end::LAND_CONTRADICTED_HEAD),
    ] {
        assert_eq!(contradiction_tag(&wrong), CONTRADICTED_WRONG_TAG, "{wrong}");
    }
    assert_eq!(contradiction_tag(LAND_NUDGE), "");
}

fn usage_reset(a: &Answered) -> i64 {
    assert_eq!(a.status, 429);
    let body = J::parse(&a.body).expect("usage response JSON");
    let reset = body
        .get("error")
        .and_then(|error| error.get("resets_at"))
        .and_then(J::as_i64)
        .expect("usage reset");
    let header = a
        .headers
        .iter()
        .find(|(key, _)| key == "x-codex-primary-reset-at")
        .and_then(|(_, value)| value.parse::<i64>().ok());
    assert_eq!(header, Some(reset));
    reset
}

fn user(t: &str) -> J {
    J::obj([("role", J::s("user")), ("content", J::s(t))])
}

fn body(texts: &[&str], tool: Option<&str>) -> J {
    let mut input: Vec<J> = texts.iter().map(|t| user(t)).collect();
    if let Some(out) = tool {
        input.push(J::obj([
            ("type", J::s("function_call_output")),
            ("output", J::s(out)),
        ]));
    }
    J::obj([("input", J::Arr(input))])
}

fn text_of(a: &Answered) -> String {
    a.body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .filter_map(|d| d.get("item").cloned())
        .map(|it| it.py())
        .collect()
}

/// aterm's acts as codex-alab types them: the base words, then a suffix.
fn acted(base: &str, suffix: &str) -> String {
    format!("{base} {suffix}")
}

#[test]
fn each_request_is_read_for_its_leg_and_the_act_aterm_s_words_name() {
    assert_eq!(
        leg_of(&body(&["<env>", "LEG-continue Write a plan"], None)).as_deref(),
        Some("continue")
    );
    assert_eq!(leg_of(&body(&["no leg here"], None)), None);
    let p = "LEG-land Land it.";
    assert_eq!(step_of(&body(&[p], None)), Step::Person);
    assert_eq!(step_of(&body(&[p, LAND], None)), Step::Land);
    let n1 = acted(LAND_NUDGE, "Your report still names open work (pushing).");
    assert_eq!(step_of(&body(&[p, LAND, &n1], None)), Step::Nudge(1));
    assert_eq!(
        step_of(&body(&[p, LAND, &n1, LAND_NUDGE, LAND_NUDGE], None)),
        Step::Nudge(3),
        "every nudge of the conversation is counted"
    );
    assert_eq!(step_of(&body(&[p, LAND_ANSWER], None)), Step::LandAnswer);
    assert_eq!(
        step_of(&body(&[p, LAND, LAND_CHECK], None)),
        Step::LandCheck
    );
    assert_eq!(step_of(&body(&[p, DONE_CHECK], None)), Step::Defend);
    assert_eq!(
        step_of(&body(&[p, &acted(CONTINUE_TEXT, NEEDS_ACK)], None)),
        Step::Continue
    );
    assert_eq!(step_of(&body(&[p, "keep going"], None)), Step::Continue);
    for line in LIMIT_CLEARED {
        assert_eq!(
            step_of(&body(&[p, &format!("[codex-alab] {line}")], None)),
            Step::Cleared
        );
    }
    assert_eq!(
        step_of(&body(
            &[
                p,
                "If the task is finished and verified, reply with ATERM-TASK-DONE"
            ],
            None
        )),
        Step::RetiredCheck
    );
    assert_eq!(
        step_of(&body(&[p, LAND, "REOPEN look again"], None)),
        Step::Reopen
    );
    assert_eq!(step_of(&body(&[p, "please continue"], None)), Step::Other);
}

#[test]
fn the_cruise_legs_claim_done_and_the_token_is_only_a_claim() {
    let first = answer(&body(&["LEG-first Say hello and finish."], None));
    assert_eq!(first.tag, "first:person");
    assert!(text_of(&first).contains(FINISHED));
    assert!(says_done(Some(FIRST_DONE)), "the token is a done claim");
    assert!(!clean_defense(Some(FIRST_DONE)), "and no landing's defense");
    let defend = answer(&body(&["LEG-first Say hello", DONE_CHECK], None));
    assert_eq!(defend.tag, "first:defend");
    assert!(
        says_done(Some(CRUISE_DEFENSE)) && clean_defense(Some(CRUISE_DEFENSE)),
        "the Cruise defense would land under Land: Cruise must still go on"
    );
    let t1 = answer(&body(&["LEG-continue Write a plan"], None));
    assert_eq!(
        (t1.tag.as_str(), t1.delay),
        ("continue:person", Duration::ZERO)
    );
    assert!(!says_done(Some(STEP_ONE)), "the first turn ends undone");
    let after = answer(&body(&["LEG-continue Write a plan", CONTINUE_TEXT], None));
    assert_eq!(after.tag, "continue:fail");
    let ran = answer(&body(
        &["LEG-continue Write a plan", CONTINUE_TEXT],
        Some("Process exited with code 3"),
    ));
    assert_eq!(ran.tag, "continue:continue");
    assert!(text_of(&ran).contains("All done."));
    assert!(says_done(Some(CONTINUED_DONE)));
    let plain = answer(&body(&["hello"], None));
    assert_eq!((plain.status, plain.tag.as_str()), (200, "-:other"));
}

/// The calls `a` answers with: each `function_call` item's name and its parsed arguments.
fn calls_of(a: &Answered) -> Vec<(String, J)> {
    a.body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .filter_map(|d| d.get("item").cloned())
        .filter(|it| it.get("type").and_then(J::as_str) == Some("function_call"))
        .map(|it| {
            let name = it.get("name").and_then(J::as_str).unwrap_or("").to_string();
            let args = it
                .get("arguments")
                .and_then(J::as_str)
                .and_then(|a| J::parse(a).ok())
                .unwrap_or(J::Null);
            (name, args)
        })
        .collect()
}

/// GAP 20'S LAB LEG: the continue leg's continuation, the Autopilot's turn, runs one command
/// that exits 3, then says it continued; the person's turn, a later continuation and every
/// other leg's continuation run nothing.
#[test]
fn the_continue_legs_continuation_runs_one_failing_command() {
    let p = "LEG-continue Write a plan";
    let fail = answer(&body(&[p, CONTINUE_TEXT], None));
    assert_eq!(fail.tag, "continue:fail");
    let called = calls_of(&fail);
    assert_eq!(called.len(), 1, "{}", fail.body);
    let (name, args) = &called[0];
    assert_eq!(name, "exec_command");
    assert_eq!(args.get("cmd").and_then(J::as_str), Some("exit 3"));
    assert!(
        matches!(args.get("login"), Some(J::Bool(false))),
        "no login shell: {}",
        args.py()
    );
    assert!(args.get("sandbox_permissions").is_none(), "{}", args.py());
    let ran = answer(&body(
        &[p, CONTINUE_TEXT],
        Some("Process exited with code 3"),
    ));
    assert_eq!(ran.tag, "continue:continue");
    assert!(calls_of(&ran).is_empty());
    assert!(text_of(&ran).contains("All done."));
    let again = answer(&body(&[p, CONTINUE_TEXT, CONTINUE_TEXT], None));
    assert_eq!(
        again.tag, "continue:continue",
        "only the first continuation fails"
    );
    assert!(calls_of(&again).is_empty());
    let person = answer(&body(&[p], None));
    assert_eq!(person.tag, "continue:person");
    assert!(calls_of(&person).is_empty());
    for leg in ["hold", "off", "control"] {
        let a = answer(&body(
            &[&format!("LEG-{leg} Write a plan"), CONTINUE_TEXT],
            None,
        ));
        assert_eq!(a.tag, format!("{leg}:continue"));
        assert!(calls_of(&a).is_empty(), "{leg}");
        assert!(text_of(&a).contains("All done."), "{leg}");
    }
}

#[test]
fn the_needs_leg_names_one_need_then_withdraws_it() {
    let t1 = answer(&body(&["LEG-needs Build the release."], None));
    assert_eq!(t1.tag, "needs:person");
    assert_eq!(needs_you(NEEDS_REPORT), vec![NEED.to_string()]);
    assert!(!says_done(Some(NEEDS_REPORT)), "a need is no done claim");
    assert_eq!(
        classify_said(Some(&without_needs(NEEDS_REPORT))),
        Said::Plain,
        "the need asks nothing of the harness: the next act is a continuation"
    );
    let acked = answer(&body(
        &["LEG-needs Build", &acted(CONTINUE_TEXT, NEEDS_ACK)],
        None,
    ));
    assert_eq!(
        (acked.tag.as_str(), acked.delay),
        ("needs:continue+ack", NEEDS_HOLD)
    );
    assert!(text_of(&acked).contains(NEEDS_PLAIN));
    assert!(
        needs_you(NEEDS_PLAIN).is_empty()
            && !says_no_needs(NEEDS_PLAIN)
            && !says_done(Some(NEEDS_PLAIN)),
        "the plain report names no need and withdraws none: the need stands"
    );
    assert_eq!(classify_said(Some(NEEDS_PLAIN)), Said::Plain);
    let later = answer(&body(
        &[
            "LEG-needs Build",
            &acted(CONTINUE_TEXT, NEEDS_ACK),
            CONTINUE_TEXT,
        ],
        None,
    ));
    assert_eq!(
        (later.tag.as_str(), later.delay),
        ("needs:continue", Duration::ZERO)
    );
    assert!(text_of(&later).contains("NEEDS YOU: nothing"));
    assert!(needs_you(NEEDS_CLEARED).is_empty() && says_no_needs(NEEDS_CLEARED));
    let bare = answer(&body(&["<env>", "LEG-needs Build", CONTINUE_TEXT], None));
    assert_eq!(bare.tag, "needs:continue");
    assert!(text_of(&bare).contains(NEEDS_PLAIN));
    assert_eq!(
        texts_after_prompt(&body(&["<env>", "LEG-needs Build"], None)),
        0
    );
    assert_eq!(texts_after_prompt(&body(&["no leg"], None)), 0);
}

#[test]
fn the_land_leg_defends_unclean_twice_then_clean_at_its_fourth_nudge() {
    let p = "LEG-land Land the work.";
    let t1 = answer(&body(&[p], None));
    assert_eq!((t1.tag.as_str(), t1.delay), ("land:person", Duration::ZERO));
    let land = answer(&body(&[p, LAND], None));
    assert_eq!((land.tag.as_str(), land.delay), ("land:land", SLOW_TURN));
    let mut conv = vec![p, LAND];
    let mut said = Vec::new();
    for n in 1..=NUDGES_BEFORE_DEFENSE {
        conv.push(LAND_NUDGE);
        let a = answer(&body(&conv, None));
        assert_eq!(a.tag, format!("land:nudge{n}"));
        assert_eq!((land_reply(n).1, a.delay), (a.delay, a.delay));
        said.push(text_of(&a));
    }
    for n in 0..NUDGES_BEFORE_DEFENSE {
        let (text, hold) = land_reply(n);
        assert!(!clean_defense(Some(text)), "reply {n}: {text}");
        assert!(
            hold == SLOW_TURN || says_done(Some(text)),
            "reply {n}: real work, or a defense whose nudge backs off"
        );
    }
    let (last, _) = land_reply(NUDGES_BEFORE_DEFENSE);
    assert!(clean_defense(Some(last)) && last == LAND_DEFENSE);
    assert!(said[NUDGES_BEFORE_DEFENSE - 1].contains("Nothing left:"));
    assert!(says_done(Some(NEEDS_DEFENSE)) && needs::defense_line(NEEDS_DEFENSE).is_some());
    assert!(!needs_you(NEEDS_DEFENSE).is_empty(), "a need keeps it open");
    assert!(says_done(Some(OPEN_DEFENSE)) && needs::defense_line(OPEN_DEFENSE).is_some());
    assert!(
        needs_you(OPEN_DEFENSE).is_empty() && says_no_needs(OPEN_DEFENSE),
        "no need: only the open work keeps it open"
    );
    assert!(clean_defense(Some(
        "Checked origin/main again.\n\nNothing left: all of the work is on origin/main.\nNEEDS YOU: nothing"
    )));
    for progress in LAND_PROGRESS {
        assert!(
            !says_done(Some(progress)) && !clean_defense(Some(progress)),
            "{progress}: no claim, so Land nudges again"
        );
        assert_eq!(classify_said(Some(progress)), Said::Plain, "{progress}");
    }
    assert!(
        SLOW_TURN > Duration::from_secs(120),
        "past codex-alab's min_work"
    );
    assert!(!clean_defense(Some(
        "Nothing left: all of the work is on origin/main.\nNEEDS YOU: the release credential"
    )));
    assert!(!clean_defense(Some(
        "Nothing left: all of the work is on origin/main, but the docs fix is not pushed."
    )));
    assert!(!clean_defense(Some(
        "Verified on the remote.\n\nNothing left: all of the work is on origin/main."
    )));
}

#[test]
fn the_reland_leg_lands_at_once_after_real_work_and_reopens_on_real_work() {
    let p = "LEG-reland Land the work.";
    let t1 = answer(&body(&[p], None));
    assert_eq!(
        (t1.tag.as_str(), t1.delay),
        ("reland:person", SLOW_TURN),
        "real work: the land act is not backed off, so its defense's is two minutes"
    );
    let land = answer(&body(&[p, LAND], None));
    assert_eq!(
        (land.tag.as_str(), land.delay),
        ("reland:land", Duration::ZERO)
    );
    assert!(text_of(&land).contains("Nothing left:"));
    let again = answer(&body(&[p, LAND, "REOPEN look once more"], None));
    assert_eq!(
        (again.tag.as_str(), again.delay),
        ("reland:reopen", SLOW_TURN),
        "the person's turn is real work: Land's next act comes at once"
    );
    let relanded = answer(&body(&[p, LAND, "REOPEN look once more", LAND], None));
    assert_eq!(relanded.tag, "reland:land");
    assert!(text_of(&relanded).contains("Nothing left:"));
}

#[test]
fn the_repository_leg_defends_clean_words_over_a_dirty_repository() {
    let p = "LEG-repo Write the notes.";
    assert_eq!(answer(&body(&[p], None)).delay, SLOW_TURN);
    assert!(!says_done(Some(REPO_WROTE)));
    let land = answer(&body(&[p, LAND], None));
    assert_eq!(
        (land.tag.as_str(), land.delay),
        ("repo:land", Duration::ZERO)
    );
    assert!(
        clean_defense(Some(REPO_DEFENSE)),
        "the words alone are clean: only the repository keeps it from landing"
    );
    let nudge = acted(
        LAND_NUDGE,
        "Your repository still holds work no remote main has (1 untracked (notes.txt)): \
         commit, merge and push it to the remote main first.",
    );
    let committed = answer(&body(&[p, LAND, &nudge], None));
    assert_eq!(
        (committed.tag.as_str(), committed.delay),
        ("repo:nudge1+repo", REPO_COMMIT_HOLD),
        "the tag says the nudge named the repository's work"
    );
    assert!(text_of(&committed).contains("Committed notes.txt."));
    assert!(
        clean_defense(Some(REPO_COMMITTED)),
        "clean words over an unpushed commit"
    );
    let unpushed = acted(
        LAND_NUDGE,
        "Your repository still holds work no remote main has (1 unpushed commit, 1 commit \
         not on origin/main): commit, merge and push it to the remote main first.",
    );
    let pushed = answer(&body(&[p, LAND, &nudge, &unpushed], None));
    assert_eq!(
        (pushed.tag.as_str(), pushed.delay),
        ("repo:nudge2+repo", REPO_PUSH_HOLD)
    );
    assert!(text_of(&pushed).contains("pushed it to origin/main"));
    let bare = answer(&body(&[p, LAND, LAND_NUDGE], None));
    assert_eq!(bare.tag, "repo:nudge1");
    assert!(clean_defense(Some(REPO_PUSHED)));
}

#[test]
fn the_question_leg_asks_in_its_overlay_then_in_prose() {
    let ask = answer(&body(&["LEG-question Set up"], None));
    assert_eq!(ask.tag, "question:ask");
    assert!(text_of(&ask).contains("request_user_input"));
    assert!(text_of(&ask).contains("Postgres (Recommended)"));
    let after = answer(&body(
        &["LEG-question Set up"],
        Some("Postgres (Recommended)"),
    ));
    assert_eq!(after.tag, "question:after-answer");
    assert!(
        matches!(
            classify_said(Some(QUESTION_PROSE)),
            Said::Question | Said::Stop(_)
        ),
        "the prose asks the person"
    );
    assert!(!says_done(Some(QUESTION_PROSE)), "a question, not a claim");
    let landed = answer(&body(&["LEG-question Set up", LAND_ANSWER], None));
    assert_eq!(landed.tag, "question:land-answer");
    let call = answer(&body(&["LEG-approval Run the build."], None));
    assert_eq!(call.tag, "approval:call");
    assert!(text_of(&call).contains("require_escalated"));
    let more = answer(&body(&["LEG-approval Run the build.", CONTINUE_TEXT], None));
    assert!(
        !text_of(&more).contains("require_escalated"),
        "no second call"
    );
    let ran = answer(&body(&["LEG-approval Run the build."], Some("done")));
    assert_eq!(ran.tag, "approval:after-output");
    let unanswered = answer(&body(&["LEG-box Run the build."], None));
    assert_eq!(
        unanswered.tag, "box:call",
        "the box leg asks for the same box"
    );
    assert!(text_of(&unanswered).contains("require_escalated"));
    let answered = answer(&body(&["LEG-box Run the build."], Some("done")));
    assert_eq!(answered.tag, "box:after-output");
}

#[test]
fn the_hold_and_carry_legs_hold_their_first_turn() {
    let long = answer(&body(&["LEG-carry Do the long task."], None));
    assert_eq!(long.tag, "carry:person");
    assert!(
        long.delay >= CARRY_FIRST_TURN && !says_done(Some("The long task's first step.")),
        "the carry leg's first turn is still running when it is killed"
    );
    let carried = answer(&body(&["LEG-carry Do the long task.", CONTINUE_TEXT], None));
    assert_eq!(
        (carried.tag.as_str(), carried.delay),
        ("carry:continue", Duration::ZERO)
    );
    assert!(says_done(Some(CARRIED_DONE)));
    let off = answer(&body(&["LEG-off Write a plan"], None));
    assert_eq!(
        (off.tag.as_str(), off.delay),
        ("off:person", SLOW_TURN),
        "real work: the act after it falls due at once, inside the Off window"
    );
    let on = answer(&body(&["LEG-off Write a plan", CONTINUE_TEXT], None));
    assert_eq!(
        (on.tag.as_str(), on.delay),
        ("off:continue", Duration::ZERO)
    );
    let held = answer(&body(&["LEG-hold Write a plan"], None));
    assert_eq!(
        (held.tag.as_str(), held.delay),
        ("hold:person", HOLD_FIRST_TURN)
    );
}

/// THE EXCHANGE LEGS are answered alike: the same plain reply, held the
/// same turn of real work, so only whose hand opened the turn can tell
/// the person's exchange (left to them) from the control's (continued).
#[test]
fn the_exchange_legs_answer_alike_a_plain_turn_of_real_work() {
    use aterm_agent::supervise::policy::turn_end::EXCHANGE_WORK;
    let asks = |leg: &str| format!("LEG-{leg} {PERSON_ASKS}");
    for leg in ["exchange", "control"] {
        let a = answer(&body(&["<env>", &asks(leg)], None));
        assert_eq!(
            (a.tag.as_str(), a.delay),
            (format!("{leg}:person").as_str(), SLOW_TURN),
            "{leg}"
        );
        assert!(text_of(&a).contains(EXCHANGE_REPLY), "{leg}");
    }
    assert!(!PERSON_ASKS.contains('?'));
    assert_eq!(
        classify_said(Some(EXCHANGE_REPLY)),
        Said::Plain,
        "a plain reply: the control's point is owed the continuation"
    );
    assert!(
        !says_done(Some(EXCHANGE_REPLY))
            && !clean_defense(Some(EXCHANGE_REPLY))
            && needs_you(EXCHANGE_REPLY).is_empty(),
        "no claim, no defense, no need: nothing in the words leaves the point"
    );
    assert!(
        SLOW_TURN > Duration::from_secs(120) && SLOW_TURN < EXCHANGE_WORK,
        "real work (the act after it is due at once), and still a person's own exchange"
    );
    let continued = answer(&body(&[&asks("control"), CONTINUE_TEXT], None));
    assert_eq!(
        (continued.tag.as_str(), continued.delay),
        ("control:continue", Duration::ZERO)
    );
    assert!(says_done(Some(CONTINUED_DONE)), "a claim Cruise defends");
    let typed = answer(&body(&[&asks("exchange"), CONTINUE_TEXT], None));
    assert_eq!(
        (typed.tag.as_str(), typed.delay),
        ("exchange:continue", Duration::ZERO)
    );
    assert_ne!(
        classify_said(Some(
            "The repository holds a README and one heading in it. Should I add a parser?"
        )),
        Said::Plain
    );
}

#[test]
fn the_usage_leg_keeps_one_reset_until_it_is_continued() {
    let clock = Arc::new(AtomicI64::new(1_000));
    let now = Arc::clone(&clock);
    let answer = answerer_with_clock(move || now.load(Ordering::Relaxed));
    let a = answer(&body(&["LEG-usage Start the task."], None));
    clock.store(2_000, Ordering::Relaxed);
    let b = answer(&body(&["LEG-usage Start the task."], None));
    assert_eq!((a.status, b.status), (429, 429));
    assert_eq!(usage_reset(&a), 1_000 + RESETS_IN_S);
    assert_eq!(usage_reset(&b), usage_reset(&a));
    assert_eq!(a.tag, b.tag, "one window: the reset never moves");
    assert!(a.body.contains("usage_limit_reached"));
    assert!(
        a.headers
            .iter()
            .any(|(k, _)| k == "x-codex-primary-used-percent")
    );
    let cleared = format!("[codex-alab] {}", LIMIT_CLEARED[0]);
    let after = answer(&body(&["LEG-usage Start the task.", &cleared], None));
    assert_eq!((after.status, after.tag.as_str()), (200, "usage:cleared"));
    // NEGATIVE CONTROL: Cruise's continuation at the reset is no resume this leg answers.
    let continued = answer(&body(&["LEG-usage Start the task.", CONTINUE_TEXT], None));
    assert_eq!(continued.tag, "usage:continue");
    assert!(!text_of(&continued).contains("Resumed after the reset."));
}

#[test]
fn independent_model_worlds_start_fresh_usage_windows_for_the_same_leg() {
    let first = answerer_with_clock(|| 1_000);
    let second = answerer_with_clock(|| 10_000);
    let request = body(&["LEG-usage Start the task."], None);
    let first_reset = usage_reset(&first(&request));
    let second_reset = usage_reset(&second(&request));
    assert_eq!(first_reset, 1_000 + RESETS_IN_S);
    assert_eq!(second_reset, 10_000 + RESETS_IN_S);
    assert!(second_reset > 10_000, "the second world's window is fresh");
    assert_eq!(usage_reset(&first(&request)), first_reset);
    assert_eq!(usage_reset(&second(&request)), second_reset);
}

#[test]
fn a_side_request_is_answered_by_its_schema_not_its_leg() {
    let mut b = body(&["LEG-usage Start the task."], None);
    b.set(
        "text",
        J::obj([(
            "format",
            J::obj([
                ("type", J::s("json_schema")),
                (
                    "schema",
                    J::obj([(
                        "properties",
                        J::obj([("title", J::obj([("type", J::s("string"))]))]),
                    )]),
                ),
            ]),
        )]),
    );
    let a = answer(&b);
    assert_eq!((a.status, a.tag.as_str()), (200, "usage:side"));
}

/// A `codex-boxes` leg asks for box `k + 1` once `k` ran, each response held on a gate of its
/// own that a retry of it waits on too, then says it is done, held by nothing.
#[test]
fn a_boxes_leg_asks_for_its_twenty_boxes_one_held_response_each() {
    let ran = |k: usize| {
        let mut input = vec![user("LEG-boxes1 Run the twenty commands, one at a time.")];
        input.extend((0..k).map(|_| {
            J::obj([
                ("type", J::s("function_call_output")),
                ("output", J::s("done")),
            ])
        }));
        answer(&J::obj([("input", J::Arr(input))]))
    };
    let first = ran(0);
    assert_eq!(first.tag, "boxes1:box01");
    let said = text_of(&first);
    assert!(
        said.contains("box-1 ") && said.contains("require_escalated"),
        "{said}"
    );
    let gate = |a: &Answered| Arc::clone(a.gate.as_ref().expect("a held response"));
    assert!(
        Arc::ptr_eq(&gate(&first), &gate(&ran(0))),
        "a retry waits on the same gate"
    );
    let last = ran(BOXES - 1);
    assert_eq!(last.tag, "boxes1:box20");
    assert!(!Arc::ptr_eq(&gate(&first), &gate(&last)), "a gate a box");
    let done = ran(BOXES);
    assert_eq!(done.tag, "boxes1:done");
    assert!(done.gate.is_none() && text_of(&done).contains("Ran all twenty commands"));
    assert_eq!(
        answer(&body(&["LEG-box Run the build."], None)).tag,
        "box:call"
    );
}

/// v6 §14.5 (5a): the model reads each of Cruise's six angled done checks, bare, as typed
/// with its suffixes and as the person's hour cuts it (`go_ahead::unless_held`), and v1's
/// fixed check as the defend step, by the base they share (`DONE_CHECK`). NEGATIVE CONTROL:
/// an angle without the base is no done check to it.
#[test]
fn every_angled_done_check_is_the_defend_step() {
    use aterm_agent::supervise::policy::turn_end::{
        DONE_ANGLES, DONE_CHECK_FIXED_BEFORE, done_check_act, done_check_words,
    };
    let p = "LEG-continue Write a plan";
    for (n, angle) in DONE_ANGLES.iter().enumerate() {
        for check in [
            done_check_words(n),
            done_check_act(n, Some("never push to main"), true),
            aterm_agent::supervise::policy::go_ahead::unless_held(&done_check_words(n), true)
                .into_owned(),
        ] {
            assert_eq!(step_of(&body(&[p, &check], None)), Step::Defend, "{check}");
        }
        assert_ne!(step_of(&body(&[p, angle], None)), Step::Defend, "{angle}");
    }
    assert_eq!(
        step_of(&body(&[p, DONE_CHECK_FIXED_BEFORE], None)),
        Step::Defend
    );
}

/// The `exec_command` line an answer asks for.
fn command_of(a: &Answered) -> String {
    a.body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|l| J::parse(l).ok())
        .find_map(|event| {
            let item = event.get("item")?;
            (item.get("type").and_then(J::as_str) == Some("function_call")).then_some(())?;
            let args = J::parse(item.get("arguments").and_then(J::as_str)?).ok()?;
            args.get("cmd").and_then(J::as_str).map(str::to_owned)
        })
        .unwrap_or_default()
}

/// codex-alab's reply mark (`replies::mark_of`, a6430a662d): each word's letters and digits, a
/// word holding a digit one `#`, nothing between them.
fn mark(said: &str) -> String {
    said.split_whitespace()
        .map(|w| {
            if w.contains(char::is_numeric) {
                "#".to_string()
            } else {
                w.chars().filter(|c| c.is_alphanumeric()).collect()
            }
        })
        .collect()
}

/// GAP 23'S FACTS LEG: the model asks for the holder's call, then the waiter's, then ends the
/// turn slowly with no done claim, and answers every continuation slowly with none, so Cruise
/// continues it at each turn end; a request whose user text carries aterm's rider is tagged
/// `+fact`, and no other is. The holder stops itself holding the lock, and the waiter prints
/// its rhythm into the ticks file (not codex's pipe), then blocks on that lock. Each of the
/// first ten continuations' replies has a mark no other has, so codex-alab never paces one as
/// a repeated report. NEGATIVE CONTROLS: the same continuation without the rider tags no fact,
/// and steps counted in digits would share one mark.
#[test]
fn the_facts_leg_runs_its_two_calls_then_its_steps_and_tags_a_fact() {
    let p = "LEG-facts Start the watcher, then work through the plan.";
    let output = |n: usize| {
        J::obj([
            ("type", J::s("function_call_output")),
            ("call_id", J::s(format!("call_{n}"))),
            ("output", J::s("Process running with session ID 1")),
        ])
    };
    let holder = answer(&body(&[p], None));
    assert_eq!(holder.tag, "facts:holder");
    let cmd = command_of(&holder);
    for want in [FACTS_LOCK, FACTS_HOLDER_PID, "LOCK_EX", "kill \"STOP\", $$"] {
        assert!(cmd.contains(want), "{want}: {cmd}");
    }
    let waiter = answer(&J::obj([("input", J::Arr(vec![user(p), output(1)]))]));
    assert_eq!(waiter.tag, "facts:waiter");
    let cmd = command_of(&waiter);
    for want in [
        FACTS_LOCK,
        FACTS_WAITER_PID,
        "LOCK_EX",
        &format!("for my $i (1..{FACTS_TICK_COUNT})"),
        "select(undef, undef, undef, 0.250)",
    ] {
        assert!(cmd.contains(want), "{want}: {cmd}");
    }
    assert!(
        cmd.starts_with("exec perl ") && cmd.ends_with(&format!(" >{FACTS_TICKS} 2>&1")),
        "the waiter is the call's leader, its rhythm in the ticks file: {cmd}"
    );
    let started = answer(&J::obj([(
        "input",
        J::Arr(vec![user(p), output(1), output(2)]),
    )]));
    assert_eq!(
        (started.tag.as_str(), started.delay),
        ("facts:person", SLOW_TURN)
    );
    assert!(text_of(&started).contains(FACTS_STARTED));
    let plain = answer(&body(&[p, CONTINUE_TEXT], None));
    assert_eq!(
        (plain.tag.as_str(), plain.delay),
        ("facts:continue", SLOW_TURN)
    );
    assert!(text_of(&plain).contains(&facts_step(1)));
    let third = answer(&body(
        &[p, CONTINUE_TEXT, CONTINUE_TEXT, CONTINUE_TEXT],
        None,
    ));
    assert!(
        text_of(&third).contains(&facts_step(3)),
        "{}",
        text_of(&third)
    );
    let steps: Vec<String> = (1..=10).map(facts_step).collect();
    let marks: std::collections::BTreeSet<String> = steps.iter().map(|s| mark(s)).collect();
    assert_eq!(marks.len(), steps.len(), "{steps:?}");
    assert_eq!(
        mark("Took step 3 of the plan."),
        mark("Took step 4 of the plan.")
    );
    let rider = " (aterm: a background call (pid 4200) has made no progress for 4 min)";
    let ridden = answer(&body(&[p, &format!("{CONTINUE_TEXT}{rider}")], None));
    assert_eq!(
        (ridden.tag.as_str(), ridden.delay),
        ("facts:continue+fact", SLOW_TURN)
    );
    for said in std::iter::once(FACTS_STARTED).chain(steps.iter().map(String::as_str)) {
        assert!(!says_done(Some(said)), "{said}");
        assert_eq!(classify_said(Some(said)), Said::Plain, "{said}");
    }
}
