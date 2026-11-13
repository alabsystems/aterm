// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::supervise::config::CONTINUE_TEXT;
use aterm_agent::supervise::policy::turn_end::{
    DONE_CHECK, RULE_SUGGESTION, done_check_words, suggestion_allowed,
};

fn reply<'a>(spec: &'a J, users: &[&str]) -> (&'a str, Option<&'a J>) {
    let users: Vec<String> = users.iter().map(|u| (*u).to_string()).collect();
    let rule = crate::stub::scripted_reply(spec, &users).expect("a rule answers");
    (
        rule.get("text").and_then(J::as_str).expect("a text"),
        rule.get("tool"),
    )
}

/// The opening of the suggestion request Claude Code 2.1.310 sends (its binary's prompt).
const SUGGESTION_ASKED: &str = "[SUGGESTION MODE: Suggest what the user might naturally \
                                type next into Claude Code.]\nFIRST: Look at the user's \
                                recent messages and original request.";

#[test]
fn the_placeholder_scene_answers_each_request_as_its_turn_needs() {
    let readme = Path::new("/lab/proj/README.md");
    let spec = Scene::Placeholder.spec(readme);
    let prompt = Scene::Placeholder.prompt();
    let (text, tool) = reply(&spec, &[&prompt]);
    assert_eq!(text, forever::WORK);
    assert_eq!(
        tool,
        Some(&forever::read_call(forever::Harness::Claude, readme)),
        "the first turn reads, so it holds the two replies a suggestion needs"
    );
    let (text, tool) = reply(&spec, &[&prompt, SUGGESTION_ASKED]);
    assert_eq!((text, tool), (SUGGESTION, None));
    assert!(
        suggestion_allowed(text),
        "a build before 5a accepted the suggestion bare"
    );
    for typed in [SUGGESTION, CONTINUE_TEXT] {
        assert_eq!(reply(&spec, &[&prompt, typed]).0, forever::DONE_CLAIM);
    }
    for check in [
        DONE_CHECK.to_string(),
        done_check_words(0),
        aterm_agent::supervise::policy::go_ahead::unless_held(&done_check_words(0), true)
            .into_owned(),
    ] {
        assert_eq!(
            reply(&spec, &[&prompt, CONTINUE_TEXT, &check]).0,
            forever::CLEAN
        );
    }
}

/// THE GO-AHEAD SCENE'S CONVERSATION: its park parks as the incident's did, on the word asked
/// for, with its NEEDS YOU line as the line the go-ahead answers; the go-ahead the policy
/// builds for it (with the acknowledgment or without) draws [`GO_ON`], which parks nothing,
/// names no need, asks nothing and claims no done, so Cruise's next act is its continuation.
/// NEGATIVE CONTROLS: the continuation and the done check, which carry the sentence that the
/// go-ahead is no need but not [`GO_AHEAD_DROP`], never draw [`GO_ON`].
#[test]
fn the_go_ahead_scene_parks_as_the_incident_did_and_goes_on_after_the_answer() {
    use aterm_agent::supervise::policy::go_ahead::{parked, unless_held};
    use aterm_agent::supervise::policy::needs::needs_you;
    use aterm_agent::supervise::policy::turn_end::{
        Said, classify_said, go_ahead_answer, says_done,
    };
    let spec = Scene::GoAhead.spec(Path::new("/lab/proj/README.md"));
    let prompt = Scene::GoAhead.prompt();
    let park = park_reply();
    assert_eq!(reply(&spec, &[&prompt]), (park.as_str(), None));
    assert!(
        park.contains(&format!("{NEEDS_YOU} say \"{PARK_WORD}\"")),
        "{park}"
    );
    let parks = parked("", &park).expect("the scene's park parks");
    assert_eq!(parks.token.as_deref(), Some(PARK_WORD));
    assert!(parks.need, "{parks:?}");
    assert_eq!(
        parks.needs,
        needs_you(&park),
        "the go-ahead answers its one line"
    );
    assert_eq!(parks.needs.len(), 1, "{parks:?}");
    for ack in [false, true] {
        let typed = go_ahead_answer(&parks, ack, None);
        assert!(
            typed.starts_with(GO_AHEAD_ANSWER)
                && typed.contains(&format!("{TOKEN_OPEN}{PARK_WORD}{TOKEN_CLOSE}"))
                && typed.contains(GO_AHEAD_DROP),
            "{typed}"
        );
        assert_eq!(reply(&spec, &[&prompt, &park, &typed]).0, GO_ON);
    }
    assert!(parked("", GO_ON).is_none() && needs_you(GO_ON).is_empty());
    assert_eq!(classify_said(Some(GO_ON)), Said::Plain);
    assert!(!says_done(Some(GO_ON)));
    for held in [false, true] {
        let continued = unless_held(CONTINUE_TEXT, held);
        assert_eq!(reply(&spec, &[&prompt, &continued]).0, forever::DONE_CLAIM);
        for n in 0..7 {
            let check = unless_held(&done_check_words(n), held).into_owned();
            assert_eq!(reply(&spec, &[&prompt, &check]).0, forever::CLEAN, "{n}");
        }
    }
}

/// The stub's request log names the go-ahead's turn by its opening, inside the 70 characters
/// it keeps of a last message, and no other text Cruise types opens so.
#[test]
fn the_go_ahead_is_told_from_every_other_cruise_text_by_its_opening() {
    let head = go_ahead_head();
    assert!(GO_AHEAD_ANSWER.starts_with(&head) && head.chars().count() <= 70);
    assert!(head.ends_with("go-ahead"), "{head}");
    let others = std::iter::once(CONTINUE_TEXT.to_string())
        .chain(std::iter::once(DONE_CHECK.to_string()))
        .chain((0..7).map(done_check_words));
    for other in others {
        assert!(!other.starts_with(&head), "{other}");
    }
}

/// A request line of the stub's log, as the stub writes one: `msgs` messages, the last
/// `last` (the 70 characters the log keeps), answered by reply rule `reply`.
fn request(msgs: usize, last: &str, reply: usize) -> String {
    let digest = crate::stub::message_digest(last);
    let last: String = last.chars().take(70).collect();
    format!(
        "1 POST /v1/messages -> 200 body=9B mode=lab kind=main model=lab-stub stream=true \
         max_tokens=32000 msgs={msgs} tools=1 sys=9c last_user=\"{last}\" auth=key \
         reply={reply} last_user_sha256={digest} custom"
    )
}

/// CLAUDE CODE TOOK THE GO-AHEAD AS ONE TURN, WHOLE, ONLY WHERE THE STUB ANSWERED IT WHOLE
/// (2026-10-30 review): the recording's shape passes, the park's request, then the answer's,
/// which the stub answered by the rule for a last message carrying [`GO_AHEAD_DROP`]
/// ([`crate::stub::scripted_rule`] names the same rules as [`spec_rule`]), then the
/// continuation's. NEGATIVE CONTROLS: the answer sent as two turns, its opening drawing the park
/// again and its last sentence [`GO_ON`], fails though exactly one request still opens with
/// the answer; so do two go-aheads, none, one after a request the park did not answer, and a
/// log that names no rule. A word inside the quoted message never stands in for the stub's.
#[test]
fn the_go_ahead_is_one_turn_whole_only_where_the_stub_answered_it_whole() {
    let spec = Scene::GoAhead.spec(Path::new("/lab/proj/README.md"));
    let prompt = Scene::GoAhead.prompt();
    let park = park_reply();
    let parks = aterm_agent::supervise::policy::go_ahead::parked("", &park).expect("the park");
    let typed = aterm_agent::supervise::policy::turn_end::go_ahead_answer(&parks, false, None);
    let users = |u: &[&str]| u.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let (drop, park_rule) = (spec_rule(GO_AHEAD_DROP), spec_rule(""));
    let cont = spec_rule(forever::CONTINUE_KEY).expect("the continuation's rule");
    assert_eq!(
        crate::stub::scripted_rule(&spec, &users(&[&prompt])),
        park_rule
    );
    assert_eq!(
        crate::stub::scripted_rule(&spec, &users(&[&prompt, &park, &typed])),
        drop
    );
    let (drop, park_rule) = (drop.expect("the drop rule"), park_rule.expect("the park's"));
    assert_ne!(drop, park_rule);
    let tail = typed
        .rsplit_once(". ")
        .map_or(typed.as_str(), |(_, t)| t)
        .to_string();
    assert!(tail.contains(GO_AHEAD_DROP), "{tail}");
    let recorded = [
        request(2, &prompt, park_rule),
        request(4, &typed, drop),
        request(6, CONTINUE_TEXT, cont),
    ];
    assert_eq!(one_turn_whole(&recorded, &typed), Ok(()));
    let split = [
        request(2, &prompt, park_rule),
        request(4, &typed, park_rule),
        request(6, &tail, drop),
    ];
    let twice = [
        request(2, &prompt, park_rule),
        request(4, &typed, drop),
        request(6, &typed, drop),
    ];
    let none = [
        request(2, &prompt, park_rule),
        request(4, CONTINUE_TEXT, cont),
    ];
    let after = [
        request(2, &prompt, park_rule),
        request(4, CONTINUE_TEXT, cont),
        request(6, &typed, drop),
    ];
    let unnamed: Vec<String> = recorded
        .iter()
        .map(|l| l.replace(&format!(" reply={drop} "), " "))
        .collect();
    let unbound: Vec<String> = recorded
        .iter()
        .map(|l| {
            l.replace(
                &format!(" last_user_sha256={}", crate::stub::message_digest(&typed)),
                "",
            )
        })
        .collect();
    for (name, lines) in [
        ("split", &split[..]),
        ("twice", &twice[..]),
        ("none", &none[..]),
        ("after", &after[..]),
        ("unnamed", &unnamed[..]),
        ("unbound", &unbound[..]),
    ] {
        assert!(one_turn_whole(lines, &typed).is_err(), "{name}");
    }
    let quoted = request(4, "x reply=9 msgs=1", drop);
    assert_eq!(
        crate::scn::word_of(&quoted, "reply="),
        Some(drop.to_string().as_str())
    );
    assert_eq!(crate::scn::word_of(&quoted, "msgs="), Some("4"));
}

#[test]
fn the_received_go_ahead_keeps_its_middle_as_well_as_its_ends() {
    let spec = go_ahead_spec();
    let prompt = Scene::GoAhead.prompt();
    let parks = aterm_agent::supervise::policy::go_ahead::parked("", &park_reply())
        .expect("the incident's park");
    let drop = spec_rule(GO_AHEAD_DROP).expect("the answer's rule");
    let park_rule = spec_rule("").expect("the park's rule");
    let mut accepted = Vec::new();
    for ack in [false, true] {
        let typed = aterm_agent::supervise::policy::turn_end::go_ahead_answer(&parks, ack, None);
        let complete = [request(2, &prompt, park_rule), request(4, &typed, drop)];
        assert_eq!(
            one_turn_whole(&complete, &typed),
            Ok(()),
            "the full answer: {ack}"
        );

        let missing_middle = format!("{} {GO_AHEAD_DROP}", go_ahead_head());
        assert!(typed.len() > missing_middle.len());
        let users = vec![prompt.clone(), missing_middle.clone()];
        assert_eq!(
            crate::stub::scripted_rule(&spec, &users),
            Some(drop),
            "the old opening and reply-marker premises still hold"
        );
        let shortened = [
            request(2, &prompt, park_rule),
            request(4, &missing_middle, drop),
        ];
        if one_turn_whole(&shortened, &typed).is_ok() {
            accepted.push(ack);
        }
    }
    assert!(
        accepted.is_empty(),
        "a received answer with its middle omitted passed: {accepted:?}"
    );
}

/// A `status` reply's shown needs. NEGATIVE CONTROLS: a refused reply reads as none, and only
/// `-` for both is no need.
#[test]
fn a_status_reply_shows_a_need_unless_both_fields_say_none() {
    let none = "OK schema=1 sid=0 needs=- needs_n=- hold_by=-";
    assert_eq!(needs_read(none).as_deref(), Some("needs=- needs_n=-"));
    assert!(!shows_need(&needs_read(none).expect("read")));
    for shown in [
        "OK needs=ask needs_n=1",
        "OK needs=ask needs_n=-",
        "OK needs=- needs_n=1",
    ] {
        assert!(shows_need(&needs_read(shown).expect("read")), "{shown}");
    }
    assert_eq!(needs_read("ERR no session"), None);
    assert_eq!(needs_read("OK needs=-"), None);
}

#[test]
fn the_land_and_cruise_scenes_play_their_forever_chains() {
    let readme = Path::new("/lab/proj/README.md");
    for (scene, chain, repo) in [
        (Scene::Landing, Chain::LandC, Chain::LandA.repo()),
        (Scene::RepoHold, Chain::LandC, Chain::LandC.repo()),
        (Scene::SameReply, Chain::Cruise, Chain::Cruise.repo()),
        (Scene::Hinted, Chain::LandHinted, Chain::LandHinted.repo()),
    ] {
        assert_eq!(scene.spec(readme), forever::spec(&[chain]), "{scene:?}");
        assert_eq!(scene.prompt(), chain.prompt(), "{scene:?}");
        assert_eq!(scene.repo(), repo, "{scene:?}");
        assert!(scene.launch().envs.is_empty(), "{scene:?}");
        let land = chain.autopilot() == "land";
        assert_eq!(
            scene.harness_keys().contains("\"land\""),
            land,
            "{scene:?}: its lab starts under its chain's Autopilot"
        );
    }
    assert_eq!(
        Scene::Placeholder.launch().envs,
        [format!("{SUGGESTIONS_ON}=1")]
    );
}

fn lines(ls: &[&str]) -> Vec<String> {
    ls.iter().map(|l| (*l).to_string()).collect()
}

#[test]
fn each_scene_ends_at_the_line_it_is_kept_for() {
    let landed = lines(&[
        "CONTINUED seq=40 rule=land@v1 Autopilot is Land: …",
        "DONE seq=61 rule=finished@v1 the worker reported ATERM-TASK-DONE",
        "CONTINUED seq=80 rule=land-nudge@v1 Still landing: …",
        "DONE seq=97 rule=land@v1 the worker defended that everything is landed",
    ]);
    assert_eq!(end_of(Scene::Landing, &landed), Some(3));
    assert_eq!(end_of(Scene::Landing, &landed[..3]), None);
    let held = lines(&[
        "EXCEPTION seq=12 kind=spend when=later a spend limit",
        "CONTINUED seq=80 rule=land-nudge@v1 Still landing: …",
        "EXCEPTION seq=97 kind=stalled when=later Its program answers aterm's acts with \
         the same reply and no work, so aterm types nothing more. => nothing",
    ]);
    assert_eq!(end_of(Scene::RepoHold, &held), Some(2));
    assert_eq!(end_of(Scene::RepoHold, &held[..2]), None);
    let recovering = lines(&[
        "CONTINUED seq=80 rule=continue-done-check@v1 review the work",
        "WAITING seq=90 repeated report; changing approach on the next turn",
        "CONTINUED seq=97 rule=continue@v1 continue autonomously",
    ]);
    assert_eq!(end_of(Scene::SameReply, &recovering), Some(2));
    assert_eq!(end_of(Scene::SameReply, &recovering[..2]), None);
    assert_eq!(end_of(Scene::SameReply, &held), None);
    let placeheld = lines(&[
        "CONTINUED seq=30 rule=continue-done-check@v1 before the suggestion",
        "CONTINUED seq=40 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
        "WAITING seq=41 4 short turn(s) in a row: 4 min back-off",
        "CONTINUED seq=55 rule=continue-done-check@v1 …",
        "NOTE seq=56 the reply is read",
        "WAITING seq=60 3 short turn(s) in a row: 8 min back-off",
    ]);
    assert_eq!(end_of(Scene::Placeholder, &placeheld), Some(5));
    assert_eq!(end_of(Scene::Placeholder, &placeheld[..5]), None);
    assert_eq!(end_of(Scene::Placeholder, &placeheld[2..]), None);
    let accepted = lines(&[
        "CONTINUED seq=40 rule=continue-suggestion@v1 keep going",
        "CONTINUED seq=55 rule=continue-done-check@v1 …",
        "WAITING seq=60 3 short turn(s) in a row: 8 min back-off",
    ]);
    assert_eq!(
        end_of(Scene::Placeholder, &accepted),
        None,
        "a suggestion accepted bare (2f's decision) is not the scene's end"
    );
    let answered = lines(&[
        "WAITING seq=20 until=… 1 short turn(s) in a row: 15 s back-off",
        "CONTINUED seq=21 rule=go-ahead@v1 [aterm harness] The owner's standing go-ahead: …",
        "EVENT idle seq=30 ⏺ Reran the checks: …",
        "WAITING seq=30 until=… 2 short turn(s) in a row: 30 s back-off",
        "SKIPPED seq=31 a draft stands",
        "CONTINUED seq=40 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
    ]);
    assert_eq!(end_of(Scene::GoAhead, &answered), Some(5));
    assert_eq!(end_of(Scene::GoAhead, &answered[..5]), None);
    // NEGATIVE CONTROLS: with the go-ahead removed (answers off escalate the park), there is
    // no end to reach; the act after it is the end whatever it is, which `play` judges.
    let escalated = lines(&[
        "ESCALATED seq=21 the worker parked its own reversible work on a person's go-ahead",
        "CONTINUED seq=40 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
    ]);
    assert_eq!(end_of(Scene::GoAhead, &escalated), None);
    let again = lines(&[
        "CONTINUED seq=21 rule=go-ahead@v1 [aterm harness] The owner's standing go-ahead: …",
        "CONTINUED seq=40 rule=go-ahead@v1 [aterm harness] The owner's standing go-ahead: …",
    ]);
    assert_eq!(end_of(Scene::GoAhead, &again), Some(1));
    assert_eq!(continued_rule(&again[1]), Some(RULE_GO_AHEAD));
}

/// A landing or a hold runs on past its next rung: c5's landing, the second short turn, a
/// 30 s back-off; c6's hold, the third, 60 s; each with the margin. NEGATIVE
/// CONTROLS: the placeholder and go-ahead scenes' ends are no quiet, and run only the settle; a
/// quiet with fewer turns waited runs the shorter rung.
#[test]
fn a_landing_or_a_hold_runs_past_the_act_it_did_not_take() {
    let margin = std::time::Duration::from_secs(QUIET_MARGIN_S);
    let s = std::time::Duration::from_secs;
    let landed = lines(&[
        "WAITING seq=9 until=… 1 short turn(s) in a row: 15 s back-off",
        "CONTINUED seq=10 rule=land@v1 Autopilot is Land: …",
    ]);
    let held = lines(&[
        "WAITING seq=9 until=… 1 short turn(s) in a row: 15 s back-off",
        "CONTINUED seq=10 rule=continue-done-check@v1 …",
        "EVENT idle seq=11 ⏺ Nothing left: …",
        "WAITING seq=11 until=… 2 short turn(s) in a row: 30 s back-off",
        "CONTINUED seq=12 rule=continue-done-check@v1 …",
    ]);
    assert_eq!(settle_after(true, &landed), s(30) + margin);
    assert_eq!(settle_after(true, &held), s(60) + margin);
    assert!(Scene::Landing.ends_quiet() && Scene::RepoHold.ends_quiet());
    assert!(!Scene::SameReply.ends_quiet());
    assert!(!Scene::Placeholder.ends_quiet() && !Scene::GoAhead.ends_quiet());
    assert_eq!(settle_after(false, &held), s(SETTLE_S));
    assert_eq!(settle_after(true, &[]), s(15) + margin);
}

/// The placeholder scene is kept only where its continuation and its done check were typed
/// over the stub's suggestion. NEGATIVE CONTROLS: none read, a person's draft, the idle hint.
#[test]
fn the_done_check_is_kept_only_over_the_suggestion() {
    assert!(over_suggestion(Some(SUGGESTION)));
    assert!(over_suggestion(Some(" keep going ")));
    for not in [
        None,
        Some(""),
        Some("fix the parser"),
        Some("Try \"refactor\""),
    ] {
        assert!(!over_suggestion(not), "{not:?}");
    }
}

#[test]
fn a_continued_line_names_its_rule_and_no_other_line_does() {
    let line = "CONTINUED seq=102 rule=continue-suggestion@v1 keep going";
    assert_eq!(continued_rule(line), Some(RULE_SUGGESTION));
    assert_eq!(continued_words(line), SUGGESTION);
    assert_eq!(continued_rule("WAITING seq=9 rule=x@v1 overloaded"), None);
}

/// The composer an act was typed over is the last one before its typing began. NEGATIVE
/// CONTROL: the suggestion it replaced is not its typing.
#[test]
fn a_composer_mid_act_is_the_acts_typing_and_the_suggestion_is_not() {
    for words in [done_check_words(0).as_str(), CONTINUE_TEXT] {
        let head: String = words.chars().take(5).collect();
        assert!(typing(&head, words));
        assert!(typing(words, words));
        assert!(!typing(SUGGESTION, words));
        assert!(!typing("", words));
    }
}

#[test]
fn each_scene_is_listed_with_its_keys_and_its_lab() {
    let listed = crate::scn::scenarios();
    for scene in Scene::ALL {
        let s = listed
            .iter()
            .find(|s| s.name == scene.name())
            .expect("listed");
        assert_eq!(s.harness_keys, scene.harness_keys(), "{scene:?}");
        assert_eq!(s.kw.instance_env, INSTANCE_ENV, "{scene:?}");
        assert!(
            matches!(s.plan, crate::scn::Plan::Autopilot(p) if p == scene),
            "{scene:?}: {:?}",
            s.plan
        );
    }
}

#[test]
fn every_scene_launches_lean() {
    for scene in Scene::ALL {
        assert_eq!(scene.launch().args, LEAN, "{scene:?}");
    }
}
