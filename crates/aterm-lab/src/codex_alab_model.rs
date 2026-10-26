// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure fake Responses model for `codex-alab` proof legs.

use std::sync::Mutex;
use std::time::Duration;

use aterm_agent::supervise::config::CONTINUE_TEXT;
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end::{
    DONE_CHECK, FINISHED, LAND, LAND_ANSWER, LAND_NUDGE,
};

use crate::codex_rig::{Answered, call_item, message_item, request_view};
use crate::pyjson::J;

#[path = "codex_alab_policy_model.rs"]
pub mod policy;

/// Legacy continuation substring shared with [`CONTINUE_TEXT`].
pub const KEEP_GOING: &str = "keep going";

/// Legacy done-check prefix recognized for negative-control classification.
pub const RETIRED_CHECK: &str = "If the task is finished and verified";

/// Prefix of the dirty-repository suffix appended to Land nudges.
pub const REPO_SUFFIX_HEAD: &str = "Your repository still holds work no remote main has";

/// Prompt text typed by a person to reopen a landed task.
pub const REOPEN: &str = "REOPEN";

/// Turn duration exceeding `codex-alab`'s two-minute `min_work` threshold.
pub const SLOW_TURN: Duration = Duration::from_secs(130);

/// Duration for which the hold leg's first response is held.
pub const HOLD_FIRST_TURN: Duration = SLOW_TURN;

/// Duration for which the carry leg's first response is held before SIGKILL.
pub const CARRY_FIRST_TURN: Duration = Duration::from_secs(3600);

/// Duration for which the repository leg holds its response while committing.
pub const REPO_COMMIT_HOLD: Duration = Duration::from_secs(60);

/// Duration for which the repository leg holds its response while pushing.
pub const REPO_PUSH_HOLD: Duration = Duration::from_secs(20);

/// Duration for which the `NEEDS YOU` leg holds its response while the need stands.
pub const NEEDS_HOLD: Duration = Duration::from_secs(10);

/// Usage-limit window duration in seconds reported by the 429 fixture.
pub const RESETS_IN_S: i64 = 5;

/// Number of Land nudges issued before the model returns a clean defense.
pub const NUDGES_BEFORE_DEFENSE: usize = 4;

/// The task `spawn agent=codex` hands the worker leg.
pub const WORKER_TASK: &str = "Write the worker's report.";

/// The worker leg's report, the words its `result` carries.
pub const WORKER_REPORT: &str = "Wrote the worker's report: the lab's task is done.";

/// First-leg reply claiming completion with the retired done token.
pub const FIRST_DONE: &str = "Hello from the lab model. Nothing else to do.\n\nATERM-TASK-DONE";

/// Cruise leg reply to the defend-and-find-more prompt.
pub const CRUISE_DEFENSE: &str = "Looked again.\n\nNothing left: the greeting was the whole task.";

/// Plain intermediate reply for the continuation leg's first turn.
pub const STEP_ONE: &str = "Here is step one of the plan. I will continue when asked.";

/// Continuation leg reply claiming completion with the retired done token.
pub const CONTINUED_DONE: &str = "Continued as asked. All done.\n\nATERM-TASK-DONE";

/// Carry leg reply after crash relaunch.
pub const CARRIED_DONE: &str = "Carried on after the relaunch. All done.\n\nATERM-TASK-DONE";

/// `NEEDS YOU` requirement text used by the needs leg.
pub const NEED: &str = "the signing credential to publish the release";

/// Initial report for the `NEEDS YOU` leg naming [`NEED`].
pub const NEEDS_REPORT: &str = "Built the release candidate and ran its checks.\n\nNEEDS YOU: the \
                                signing credential to publish the release";

/// Follow-up report for the `NEEDS YOU` leg with no `NEEDS YOU:` line.
pub const NEEDS_PLAIN: &str = "Carried on: the changelog is updated and the docs are next.";

/// Final report for the `NEEDS YOU` leg withdrawing the need (`NEEDS YOU: nothing`).
pub const NEEDS_CLEARED: &str = "Carried on without it: the changelog and the docs are \
                                 updated.\n\nNEEDS YOU: nothing";

/// In-progress replies for the Land leg before its defense.
pub const LAND_PROGRESS: [&str; 2] = [
    "Committed the change; the push to origin/main is still running.",
    "The push finished; checking that origin/main has every commit.",
];

/// Unclean Land defense that still carries a standing `NEEDS YOU:` line.
pub const NEEDS_DEFENSE: &str = "Checked origin/main.\n\nNothing left: all of the work is on \
                                 origin/main.\nNEEDS YOU: the release credential to tag it";

/// Unclean Land defense that mentions work left for later.
pub const OPEN_DEFENSE: &str = "Checked origin/main again.\n\nNothing left: all of the work is \
                                on origin/main; the docs fix is for later.\nNEEDS YOU: nothing";

/// Clean Land defense for a non-repository workspace.
pub const LAND_DEFENSE: &str = "Checked origin/main once more.\n\nNothing left: all of the work is \
                                on origin/main.\nNEEDS YOU: nothing";

/// Initial repository-leg report after writing an uncommitted file.
pub const REPO_WROTE: &str = "Wrote notes.txt with the release notes.";

/// Repository-leg defense while uncommitted changes remain.
pub const REPO_DEFENSE: &str = "The notes are written.\n\nNothing left: everything is on \
                                origin/main.";

/// Repository-leg defense while an unpushed commit remains.
pub const REPO_COMMITTED: &str = "Committed notes.txt.\n\nNothing left: everything is on \
                                  origin/main.";

/// Clean repository-leg defense after pushing to `origin/main`.
pub const REPO_PUSHED: &str = "Committed notes.txt and pushed it to origin/main.\n\nNothing left: \
                               everything is on origin/main.";

/// Prose question returned by the question leg after its overlay is answered.
pub const QUESTION_PROSE: &str = "Postgres is set up locally. Should I push the schema to \
                                  origin/main now?";

/// Question-leg reply after receiving Land's answer.
pub const QUESTION_LANDED_WORDS: &str = "Pushed the schema to origin/main.";

/// User question text shared by the `exchange` and `control` legs.
pub const PERSON_ASKS: &str = "tell me what this repository holds so far";

/// Plain reply returned for the `exchange` and `control` legs.
pub const EXCHANGE_REPLY: &str = "The repository holds a README and one heading in it.";

/// Conversation step classified from a request's last user text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Initial or subsequent person prompt.
    Person,
    /// Person turn reopening a landed task ([`REOPEN`]).
    Reopen,
    /// Cruise continuation prompt (`continue@v1`).
    Continue,
    /// Cruise defend-and-find-more prompt (`continue-done-check@v1`).
    Defend,
    /// Legacy `codex-alab` done-check prompt.
    RetiredCheck,
    /// Land instruction (`land@v1`).
    Land,
    /// `n`-th Land nudge (`land-nudge@v1`) in the conversation.
    Nudge(usize),
    /// Land answer to a prose question (`land-answer@v1`).
    LandAnswer,
    /// Any other harness-typed prompt.
    Other,
}

impl Step {
    /// Returns the step tag used in model log entries.
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Step::Person => "person".to_string(),
            Step::Reopen => "reopen".to_string(),
            Step::Continue => "continue".to_string(),
            Step::Defend => "defend".to_string(),
            Step::RetiredCheck => "retired-check".to_string(),
            Step::Land => "land".to_string(),
            Step::Nudge(n) => format!("nudge{n}"),
            Step::LandAnswer => "land-answer".to_string(),
            Step::Other => "other".to_string(),
        }
    }
}

/// The user texts of a request, in order.
fn user_texts(body: &J) -> Vec<String> {
    body.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|it| it.get("role").and_then(J::as_str) == Some("user"))
        .map(crate::stub::item_text)
        .collect()
}

/// Extracts the `<name>` from the first `LEG-<name>` user text in `body`.
#[must_use]
pub fn leg_of(body: &J) -> Option<String> {
    user_texts(body).iter().find_map(|text| {
        let (_, rest) = text.split_once("LEG-")?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        (!name.is_empty()).then_some(name)
    })
}

/// Classifies the [`Step`] of `body` from its user texts.
#[must_use]
pub fn step_of(body: &J) -> Step {
    let texts = user_texts(body);
    let (last, _) = request_view(body);
    if last.contains(LAND_NUDGE) {
        return Step::Nudge(texts.iter().filter(|t| t.contains(LAND_NUDGE)).count());
    }
    if last.contains(LAND) {
        return Step::Land;
    }
    if last.contains(LAND_ANSWER) {
        return Step::LandAnswer;
    }
    if last.contains(DONE_CHECK) {
        return Step::Defend;
    }
    if last.contains(RETIRED_CHECK) {
        return Step::RetiredCheck;
    }
    if last.contains(CONTINUE_TEXT) || last.contains(KEEP_GOING) {
        return Step::Continue;
    }
    if last.contains(REOPEN) {
        return Step::Reopen;
    }
    if last.contains("LEG-") {
        return Step::Person;
    }
    Step::Other
}

/// Returns the number of user texts following the initial `LEG-` prompt.
#[must_use]
pub fn texts_after_prompt(body: &J) -> usize {
    let texts = user_texts(body);
    texts
        .iter()
        .position(|t| t.contains("LEG-"))
        .map_or(0, |at| texts.len() - at - 1)
}

/// Returns the Land leg's `(reply_text, hold_duration)` for land act `n`.
#[must_use]
pub const fn land_reply(n: usize) -> (&'static str, Duration) {
    match n {
        0 => (LAND_PROGRESS[0], SLOW_TURN),
        1 => (NEEDS_DEFENSE, Duration::ZERO),
        2 => (LAND_PROGRESS[1], SLOW_TURN),
        3 => (OPEN_DEFENSE, Duration::ZERO),
        _ => (LAND_DEFENSE, Duration::ZERO),
    }
}

/// The usage leg's window: its reset, fixed at its first request.
static RESETS: Mutex<Vec<(String, i64)>> = Mutex::new(Vec::new());

fn reset_of(leg: &str) -> i64 {
    let mut resets = RESETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, at)) = resets.iter().find(|(l, _)| l == leg) {
        return *at;
    }
    let at = crate::util::now_secs() + RESETS_IN_S;
    resets.push((leg.to_string(), at));
    at
}

/// The question leg's overlay: one question, its second option marked
/// `(Recommended)`.
fn question_call() -> J {
    let option = |label: &str, description: &str| {
        J::obj([("label", J::s(label)), ("description", J::s(description))])
    };
    call_item(
        "request_user_input",
        &J::obj([(
            "questions",
            J::Arr(vec![J::obj([
                ("id", J::s("db_choice")),
                ("header", J::s("Database")),
                ("question", J::s("Which database should I set up?")),
                (
                    "options",
                    J::Arr(vec![
                        option("SQLite", "A single local file."),
                        option("Postgres (Recommended)", "A server, closer to production."),
                    ]),
                ),
            ])]),
        )]),
    )
}

/// The approval and box legs' call: an escalated `exec_command` under `on-request`.
fn approval_call() -> J {
    call_item(
        "exec_command",
        &J::obj([
            ("cmd", J::s("echo approved-run >> ran.txt; echo done")),
            ("sandbox_permissions", J::s("require_escalated")),
            ("justification", J::s("write the marker file")),
        ]),
    )
}

/// Computes the fake model's [`Answered`] response for `body`.
#[must_use]
pub fn answer(body: &J) -> Answered {
    let leg = leg_of(body).unwrap_or_else(|| "-".to_string());
    if let Some(answer) = policy::answer(body, &leg) {
        return answer;
    }
    let say = |text: &str| Answered::items(vec![message_item(text)]);
    let slow = |text: &str, delay: Duration| {
        let mut a = say(text);
        a.delay = delay;
        a
    };
    let format = body.get("text").and_then(|t| t.get("format"));
    if format.and_then(|f| f.get("type")).and_then(J::as_str) == Some("json_schema") {
        let schema = format.and_then(|f| f.get("schema"));
        let mut a = say(&crate::stub::schema_answer(schema));
        a.tag = format!("{leg}:side");
        return a;
    }
    let (user, tool) = request_view(body);
    let step = step_of(body);
    let mut name = step.name();
    if user.contains(NEEDS_ACK) {
        name.push_str("+ack");
    }
    if user.contains(REPO_SUFFIX_HEAD) {
        name.push_str("+repo");
    }
    let mut a = match (leg.as_str(), step) {
        ("first", Step::Person) => say(FIRST_DONE),
        ("worker", Step::Person) => say(WORKER_REPORT),
        ("needs", Step::Person) => say(NEEDS_REPORT),
        ("needs", Step::Continue | Step::Other) if texts_after_prompt(body) == 1 => {
            slow(NEEDS_PLAIN, NEEDS_HOLD)
        }
        ("needs", Step::Continue | Step::Other) => say(NEEDS_CLEARED),
        ("continue", Step::Person) => say(STEP_ONE),
        ("hold", Step::Person) => slow(STEP_ONE, HOLD_FIRST_TURN),
        ("off", Step::Person) => slow(STEP_ONE, SLOW_TURN),
        ("exchange" | "control", Step::Person) => slow(EXCHANGE_REPLY, SLOW_TURN),
        ("continue" | "hold" | "off" | "control", Step::Continue) => say(CONTINUED_DONE),
        ("carry", Step::Person) => slow("The long task's first step.", CARRY_FIRST_TURN),
        ("carry", Step::Continue) => say(CARRIED_DONE),
        ("approval" | "box", _) if tool.is_some() => {
            name = "after-output".to_string();
            say("The command ran.")
        }
        ("approval" | "box", Step::Person) => {
            name = "call".to_string();
            Answered::items(vec![approval_call()])
        }
        ("question", _) if tool.is_some() => {
            name = "after-answer".to_string();
            say(QUESTION_PROSE)
        }
        ("question", Step::Person) => {
            name = "ask".to_string();
            Answered::items(vec![question_call()])
        }
        ("question", Step::LandAnswer) => say(QUESTION_LANDED_WORDS),
        ("usage", Step::Person) => {
            let reset = reset_of(&leg);
            name = format!("limit resets_at={reset}");
            Answered {
                status: 429,
                kind: "application/json",
                body: crate::stub::usage_limit_body("five_hour", reset, Some("plus")).py(),
                headers: crate::stub::usage_limit_headers("five_hour", reset),
                delay: Duration::ZERO,
                tag: String::new(),
                gate: None,
            }
        }
        ("usage", Step::Continue) => say("Resumed after the reset."),
        ("land", Step::Person) => say("Started on the landing."),
        ("land", Step::Land) => {
            let (text, hold) = land_reply(0);
            slow(text, hold)
        }
        ("land", Step::Nudge(n)) => {
            let (text, hold) = land_reply(n);
            slow(text, hold)
        }
        ("reland", Step::Person) => slow("Started on the landing.", SLOW_TURN),
        ("reland", Step::Reopen) => slow("Looked again at the landed work.", SLOW_TURN),
        ("repo", Step::Person) => slow(REPO_WROTE, SLOW_TURN),
        ("repo", Step::Land) => say(REPO_DEFENSE),
        ("repo", Step::Nudge(1)) => slow(REPO_COMMITTED, REPO_COMMIT_HOLD),
        ("repo", Step::Nudge(_)) => slow(REPO_PUSHED, REPO_PUSH_HOLD),
        (_, Step::Defend | Step::RetiredCheck) => say(CRUISE_DEFENSE),
        (_, Step::Land | Step::Nudge(_)) => say(LAND_DEFENSE),
        (_, Step::LandAnswer) => say("Took the option that lands it soonest."),
        (_, Step::Continue) => say("Kept going: the next step is in hand."),
        _ => say(&format!(
            "Hello from the lab model. Nothing else to do.\n\n{FINISHED}"
        )),
    };
    a.tag = format!("{leg}:{name}");
    a
}

#[cfg(test)]
mod tests {
    use aterm_agent::supervise::policy::needs::{self, needs_you, says_no_needs, without_needs};
    use aterm_agent::supervise::policy::turn_end::{Said, classify_said, clean_defense, says_done};

    use super::*;

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
        assert_eq!(step_of(&body(&[p, DONE_CHECK], None)), Step::Defend);
        assert_eq!(
            step_of(&body(&[p, &acted(CONTINUE_TEXT, NEEDS_ACK)], None)),
            Step::Continue
        );
        assert_eq!(step_of(&body(&[p, "keep going"], None)), Step::Continue);
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
        assert_eq!(after.tag, "continue:continue");
        assert!(says_done(Some(CONTINUED_DONE)));
        let plain = answer(&body(&["hello"], None));
        assert_eq!((plain.status, plain.tag.as_str()), (200, "-:other"));
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
        let a = answer(&body(&["LEG-usage Start the task."], None));
        let b = answer(&body(&["LEG-usage Start the task."], None));
        assert_eq!((a.status, b.status), (429, 429));
        assert_eq!(a.tag, b.tag, "one window: the reset never moves");
        assert!(a.body.contains("usage_limit_reached"));
        assert!(
            a.headers
                .iter()
                .any(|(k, _)| k == "x-codex-primary-used-percent")
        );
        let after = answer(&body(&["LEG-usage Start the task.", CONTINUE_TEXT], None));
        assert_eq!((after.status, after.tag.as_str()), (200, "usage:continue"));
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

    /// v6 §14.5 (5a): the model reads each of Cruise's six angled done checks, bare and as typed
    /// with its suffixes, and v1's fixed check as the defend step, by the base they share
    /// (`DONE_CHECK`). NEGATIVE CONTROL: an angle without the base is no done check to it.
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
}
