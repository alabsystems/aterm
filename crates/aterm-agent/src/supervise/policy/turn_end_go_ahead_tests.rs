// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE OWNER'S STANDING GO-AHEAD (go-ahead@v1; owner, 2026-10-29: "how dare you block on me for
// go ahead! fix the cruise harness so that this cannot happen again"): a park on a person's
// go-ahead for the agent's own reversible work gets it, under Cruise and Land, and nowhere else.

use super::super::go_ahead::{
    GO_AHEAD_ANSWER, GO_AHEAD_DROP, GO_AHEAD_OTHERS, GO_AHEAD_ROOM, NEVER_A_NEED, TOKEN_CLOSE,
    TOKEN_OPEN,
};
use crate::supervise::policy::exception::{Urgency, exception_for};

/// The incident's last reply (2026-10-29), two days into the park.
const INCIDENT: &str = "Nothing has changed. The build is waiting only for your go-ahead.\n\nNEEDS YOU: \
                        say \"rerun the checks\" so I can build and test the upgrade fix branch, push \
                        it to main and remove the worktree.";

const SECS: Duration = Duration::from_secs(1);

fn go_ahead_text(a: &TurnEndAction) -> &str {
    match a {
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_GO_AHEAD => text,
        other => panic!("the owner's standing go-ahead, not {other:?}"),
    }
}

fn is_go_ahead(a: &TurnEndAction) -> bool {
    a.rule_id() == Some(RULE_GO_AHEAD)
}

/// `REVERSIBLE_ANSWER`, whatever clauses follow it.
fn answers_reversibly(a: &TurnEndAction) -> bool {
    a.rule_id() == Some(RULE_ANSWER)
        && matches!(a, TurnEndAction::Type { text, .. } if text.starts_with(REVERSIBLE_ANSWER))
}

#[test]
fn the_incident_reply_gets_the_owners_standing_go_ahead_with_its_word() {
    let mut st = TurnEndState::default();
    let a = act(&mut st, &idle(INCIDENT, Some(3 * MIN)), t0());
    let text = go_ahead_text(&a);
    // The line it answers is never on the menu bar (§15.6): no `Needs on screen`.
    assert_eq!(
        text,
        format!("{GO_AHEAD_ANSWER} {TOKEN_OPEN}rerun the checks{TOKEN_CLOSE} {GO_AHEAD_DROP}")
    );
    assert!(!text.contains(GO_AHEAD_ROOM) && !text.contains(GO_AHEAD_OTHERS));
    assert!(text.encode_utf16().count() <= VERBATIM_UNITS, "{text}");
}

#[test]
fn a_park_on_a_need_alone_or_in_an_earlier_message_gets_the_go_ahead() {
    let need = "NEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare. I'll run the \
                full verify on the tip, then push to main and clean up the worktree and branch.";
    let mut st = TurnEndState::default();
    let text = go_ahead_text(&act(&mut st, &idle(need, Some(3 * MIN)), t0())).to_string();
    assert!(
        text.contains(GO_AHEAD_ROOM) && text.contains(GO_AHEAD_DROP),
        "{text}"
    );
    let earlier = TurnEndReading {
        turn_said: Some(need.to_string()),
        ..idle("Fetched main again; nothing moved.", Some(3 * MIN))
    };
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&act(&mut st, &earlier, t0())));
    let prose = "The build was stopped for low memory, so I won't start it again unless you ask.";
    let mut st = TurnEndState::default();
    let text = go_ahead_text(&act(&mut st, &idle(prose, Some(3 * MIN)), t0())).to_string();
    assert!(
        text.contains(&format!("You parked on: \"{prose}\".")) && text.contains(GO_AHEAD_ROOM),
        "{text}"
    );
    assert!(!text.contains(GO_AHEAD_DROP), "no need to drop: {text}");
}

/// A NEED THE TURN'S LAST WORD RETRACTED PARKS NO GO-AHEAD (the second review, 2026-11-01): an
/// earlier message asks the go-ahead, then the agent did that work itself and said `NEEDS YOU:
/// nothing` (in its last message, or in an earlier one), so a go-ahead for it would be a prompt
/// it cannot act on: none is read and none typed, under Cruise and under Land, at a turn end and
/// at a break of its background work. CONTROL: the same need unretracted gets the go-ahead
/// (`a_park_on_a_need_alone_or_in_an_earlier_message_gets_the_go_ahead`). Negative control: the
/// go-ahead reader reading the earlier messages raw (`turn_go_ahead` without
/// `needs::retracted_cut`) parks on the retracted need and types `go-ahead@v1`.
#[test]
fn a_need_the_turns_last_word_retracted_parks_no_go_ahead() {
    let need = "NEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare. I'll run the \
                full verify on the tip, then push to main and clean up the worktree and branch.";
    let did = "The Mac had memory again, so I ran the full verify on the tip and pushed to main.";
    for (before, last) in [
        (need.to_string(), format!("{did}\n\nNEEDS YOU: nothing")),
        (
            format!("{need}\n\n{did} NEEDS YOU: nothing"),
            "Removed the worktree and the branch.".to_string(),
        ),
    ] {
        let r = TurnEndReading {
            turn_said: Some(before.clone()),
            ..idle(&last, Some(3 * MIN))
        };
        assert_eq!(r.go_ahead(), None, "{before:?} / {last:?}");
        let st = TurnEndState::default();
        assert_eq!(
            st.go_ahead_at_break(Some(&before), &last, false),
            None,
            "at a break: {before:?}"
        );
        let mut st = TurnEndState::default();
        let a = act(&mut st, &r, t0());
        assert!(!is_go_ahead(&a), "Cruise: {a:?}");
        let mut st = TurnEndState::default();
        assert_eq!(
            act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0()),
            land_instruction()
        );
        let a = act(&mut st, &land(r.clone()), t0() + 3 * MIN);
        assert!(!is_go_ahead(&a), "Land: {a:?}");
    }
}

#[test]
fn a_go_ahead_beside_a_persons_needs_leaves_them_standing() {
    let mixed = "The Mac is still swapping.\n\nNEEDS YOU: say \"rerun the checks\" when the Mac \
                 has memory to spare.\nNEEDS YOU: sign agy and Muse in on this Mac (browser \
                 OAuth).";
    let mut st = TurnEndState::default();
    let text = go_ahead_text(&act(&mut st, &idle(mixed, Some(3 * MIN)), t0())).to_string();
    assert!(
        text.contains(GO_AHEAD_OTHERS) && text.contains(NEEDS_ACK),
        "{text}"
    );
    assert_eq!(
        st.standing_needs().len(),
        2,
        "both needs stand on the menu bar"
    );
}

#[test]
fn every_go_ahead_fits_the_typed_budget() {
    let long = format!(
        "NEEDS YOU: say \"{}\" when the disk has room, so I can rebuild.\nNEEDS YOU: the release \
         credential.",
        "rerun the checks ".repeat(20)
    );
    let mut st = TurnEndState::default();
    let r = TurnEndReading {
        rules: Some("never push to main without the checks; ".repeat(40)),
        ..idle(&long, Some(3 * MIN))
    };
    let text = go_ahead_text(&act(&mut st, &r, t0())).to_string();
    assert!(text.encode_utf16().count() <= VERBATIM_UNITS, "{text}");
    for part in [GO_AHEAD_ROOM, GO_AHEAD_DROP, GO_AHEAD_OTHERS, NEEDS_ACK] {
        assert!(text.contains(part), "{part}: {text}");
    }
    assert!(
        text.contains(&format!("{TOKEN_OPEN}rerun the checks")),
        "{text}"
    );
}

#[test]
fn a_turn_a_person_prompted_keeps_todays_act() {
    let now = t0();
    let theirs = TurnEndReading {
        person: Some(8 * SECS),
        key: Some(8 * SECS),
        ..idle("OK, I won't push until you say so.", Some(5 * SECS))
    };
    let mut st = TurnEndState::default();
    let _ = at(&mut st, &theirs, now);
    let later = TurnEndReading {
        person: Some(3 * MIN),
        key: Some(3 * MIN),
        ..theirs
    };
    let a = decide_turn_end(&st, &later, &cfg(), now + 3 * MIN);
    assert!(
        matches!(&a, TurnEndAction::Type { .. }) && !is_go_ahead(&a),
        "the person's hold stands: {a:?}"
    );
    // Nor does the continuation say the go-ahead is no need (2026-10-29 review).
    assert!(
        matches!(&a, TurnEndAction::Type { text, .. } if !text.contains(NEVER_A_NEED)),
        "{a:?}"
    );
}

#[test]
fn an_irreversible_ask_keeps_the_reversible_answer() {
    // The reader parks no turn whose prose names a force-push (2026-10-29 review), and the
    // irreversible guard stands behind it: a park the reader allows (`No force-push`) beside the
    // base's irreversible reading keeps `REVERSIBLE_ANSWER` too.
    let said = "Should I force-push the old branch to the fork? Separately, the docs are fine. \
                The build is waiting for your go-ahead.";
    let r = idle(said, Some(3 * MIN));
    assert!(r.go_ahead().is_none(), "the force-push excludes the park");
    assert!(r.asked().is_some_and(|(_, irreversible)| irreversible));
    let mut st = TurnEndState::default();
    assert_eq!(act(&mut st, &r, t0()), answered_reversibly());
    let negated = idle(
        "No force-push and nothing published. I'm still waiting for your go-ahead before \
         rebuilding.",
        Some(3 * MIN),
    );
    assert!(negated.go_ahead().is_some(), "the reader parks it");
    let mut st = TurnEndState::default();
    assert_eq!(act(&mut st, &negated, t0()), answered_reversibly());
}

#[test]
fn answers_off_escalate_a_park_and_off_types_nothing() {
    let mut st = TurnEndState::default();
    let r = idle(INCIDENT, Some(3 * MIN));
    match at_under(&mut st, &r, t0(), &no_answers()) {
        TurnEndAction::Escalate { reason } => {
            assert_eq!(reason, format!("{GO_AHEAD_WHY}; {ANSWERS_OFF}"));
        }
        other => panic!("escalated: {other:?}"),
    }
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &under(Autopilot::Off, r), t0()),
        TurnEndAction::Nothing
    );
}

#[test]
fn land_gives_the_go_ahead_only_after_its_instruction_and_over_unlanded_work() {
    let now = t0();
    let mut st = TurnEndState::default();
    let first = act(
        &mut st,
        &land(idle("Started on the parser.", Some(3 * MIN))),
        now,
    );
    assert_eq!(
        first.rule_id(),
        Some(RULE_LAND),
        "the instruction comes first"
    );
    let bare = land(idle(
        "Nothing has changed. The build is waiting only for your go-ahead.",
        Some(3 * MIN),
    ));
    let mut clean = st.clone();
    let a = act(&mut clean, &bare, now + 10 * MIN);
    assert!(!is_go_ahead(&a), "nothing unlanded: Land's own act: {a:?}");
    let over = over_repo(bare, uncommitted_repo());
    let a = act(&mut st.clone(), &over, now + 10 * MIN);
    assert!(is_go_ahead(&a), "{a:?}");
    // The incident's branch lived in a worktree of its own, which the session's folder does not
    // show: a park that names its branch or worktree is unlanded work by its own words.
    let parked = land(idle(INCIDENT, Some(3 * MIN)));
    let a = act(&mut st, &parked, now + 10 * MIN);
    assert!(is_go_ahead(&a), "{a:?}");
}

/// LAND TYPES ITS INSTRUCTION FIRST OVER A PARK (2026-10-29 review 4): a switch to Land onto a
/// reply parked on the go-ahead gets the instruction at once, never Land's answer in its place,
/// which would leave the go-ahead waiting on an instruction that never comes; the park after it
/// gets the go-ahead. With answers off it is still told.
#[test]
fn land_types_its_instruction_first_over_a_park() {
    let parked = land(idle(INCIDENT, Some(3 * MIN)));
    let mut st = TurnEndState::default();
    let first = act(&mut st, &parked, t0());
    assert_eq!(first.rule_id(), Some(RULE_LAND), "{first:?}");
    let (a, _) = until_acted(&mut st, &parked, t0() + 10 * MIN);
    assert!(is_go_ahead(&a), "{a:?}");
    assert!(matches!(
        at_under(&mut TurnEndState::default(), &parked, t0(), &no_answers()),
        TurnEndAction::Escalate { .. }
    ));
}

/// A SLEEP IS NO PARK STANDING (2026-10-30 review 9, second pass): the go-ahead went once, the
/// agent answered it in half a minute with the park reworded (no same reply holds it), and the
/// lid closed inside the back-off and opened nine hours later. The sleep is the Mac's wait, not
/// the agent's on its park, as a wall's is: the wake's decision types the second go-ahead, never
/// the refusal, and the span runs again from that act.
#[test]
fn a_sleep_after_a_go_ahead_is_no_park_standing() {
    let now = t0();
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&act(
        &mut st,
        &idle(INCIDENT, Some(3 * MIN)),
        now
    )));
    let reworded = idle(
        "Memory still low. NEEDS YOU: say \"rerun the checks\" when memory frees.",
        Some(30 * SECS),
    );
    let a = act(&mut st, &reworded, now + 40 * SECS);
    assert!(
        matches!(
            a,
            TurnEndAction::WaitUntil {
                wait: WaitKind::BackOff { .. },
                ..
            }
        ),
        "PRECONDITION: the back-off after the act: {a:?}"
    );
    st.slept(9 * 60 * MIN);
    let woke = now + 5 * MIN;
    let again = TurnEndReading {
        worked: None,
        ..reworded
    };
    let a = act(&mut st, &again, woke);
    assert!(is_go_ahead(&a), "the second go-ahead at the wake: {a:?}");
    assert_ne!(
        exception_for(&st, &again, &cfg(), woke),
        Some(crate::supervise::policy::exception::Exception::refuses_go_ahead()),
        "nothing refused"
    );
}

/// A WALL'S WAIT IS NO PARK STANDING (2026-10-30 review 9): the turn the go-ahead opened ran
/// half a minute into an outage, and the park after it, two hours after the act, gets the
/// go-ahead again, not the refusal.
#[test]
fn a_park_after_a_wall_gets_the_go_ahead_again() {
    let stood = "NEEDS YOU: say \"rerun the checks\" so I can build and test bedb9db472 and push \
                 it to main.";
    let now = t0();
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&act(
        &mut st,
        &idle(INCIDENT, Some(3 * MIN)),
        now
    )));
    let _ = act(
        &mut st,
        &walled(WallKind::Overloaded, "529", Some(30 * SECS)),
        now + MIN,
    );
    let (a, _) = until_acted(
        &mut st,
        &idle(stood, Some(30 * SECS)),
        now + 2 * GO_AHEAD_SPAN,
    );
    assert!(is_go_ahead(&a), "{a:?}");
}

#[test]
fn the_agents_own_wakes_starve_no_go_ahead() {
    // The incident's first twenty hours: the agent woke itself every 59 minutes, each wake a new
    // point that started the short-turn ladder's 60-minute wait again.
    let mut now = t0();
    let mut st = TurnEndState::default();
    for k in 0..6 {
        now += 59 * MIN;
        let _ = at(
            &mut st,
            &idle(
                &format!("Checked again ({k}); nothing moved."),
                Some(30 * SECS),
            ),
            now,
        );
    }
    assert!(
        st.short_wait().is_some_and(|w| w > GO_AHEAD_WAIT),
        "{:?}",
        st.short_wait()
    );
    now += 59 * MIN;
    let parked = idle(
        "The build is waiting only for your go-ahead (wake 7).",
        Some(30 * SECS),
    );
    let (a, then) = until_acted(&mut st, &parked, now);
    assert!(
        is_go_ahead(&a) && then <= now + GO_AHEAD_WAIT,
        "{a:?} at +{:?}",
        then - now
    );
    // The next wake parks again: its wait runs from the act typed, so it is due at once.
    let next = now + 59 * MIN;
    let again = idle(
        "The build is waiting only for your go-ahead (wake 8).",
        Some(30 * SECS),
    );
    let a = act(&mut st, &again, next);
    assert!(is_go_ahead(&a), "{a:?}");
}

/// Every text the harness types about the go-ahead: none parks, stops, names a need or claims
/// done, so an agent that quotes it asks nothing.
#[test]
fn the_go_ahead_texts_read_as_nothing() {
    for said in harness_texts() {
        assert!(
            super::super::go_ahead::parked("", &said).is_none(),
            "parks: {said}"
        );
        assert!(
            !matches!(
                classify_said(Some(&said)),
                Said::Stop(_) | Said::Irreversible(_)
            ),
            "stops: {said}: {:?}",
            classify_said(Some(&said))
        );
        assert!(needs::needs_you(&said).is_empty(), "a need: {said}");
        assert!(!says_done(Some(&said)), "done: {said}");
    }
    // The continuation, the carry and every done check say the go-ahead is no need in words a
    // person's hold cuts.
    let mut saying: Vec<String> = [
        crate::supervise::config::CONTINUE_TEXT,
        crate::supervise::policy::cruise_carry::BACKGROUND_CARRY,
        DONE_TAIL,
    ]
    .iter()
    .map(|t| (*t).to_string())
    .collect();
    saying.extend((0..DONE_ANGLES.len()).map(done_check_words));
    for said in &saying {
        assert!(said.contains(NEVER_A_NEED), "{said}");
        let cut = super::super::go_ahead::unless_held(said, true);
        assert!(!cut.contains(NEVER_A_NEED) && !cut.contains("  "), "{cut}");
    }
}

/// What the harness types or its docs say about the go-ahead: the answer and each of its parts,
/// the answer as typed to the incident, the continuation, the carry and the turn-end texts beside
/// them, and the go-ahead's sentences in the primer, the supervise skill and the manual.
fn harness_texts() -> Vec<String> {
    use super::super::go_ahead::{QUOTE_CLOSE, QUOTE_OPEN};
    let quoted = go_ahead_answer(
        &super::super::go_ahead::parked("", INCIDENT).expect("parks"),
        true,
        None,
    );
    let mut texts: Vec<String> = [
        GO_AHEAD_ANSWER,
        GO_AHEAD_ROOM,
        GO_AHEAD_DROP,
        GO_AHEAD_OTHERS,
        NEVER_A_NEED,
        TOKEN_OPEN,
        TOKEN_CLOSE,
        QUOTE_OPEN,
        QUOTE_CLOSE,
        crate::supervise::config::CONTINUE_TEXT,
        crate::supervise::policy::cruise_carry::BACKGROUND_CARRY,
        DONE_TAIL,
        NEEDS_ACK,
        needs::NEEDS_HINT,
        "how dare you block on me for go ahead!",
    ]
    .iter()
    .map(|t| (*t).to_string())
    .collect();
    texts.push(quoted);
    let docs = [
        aterm_primer::primer_block(None),
        include_str!("../../../../aterm-primer/assets/supervise-agent-skill.md").to_string(),
        include_str!("../../../../aterm-cli/src/manual.rs").to_string(),
    ];
    for doc in docs {
        // A Rust literal's escapes and continuations read as the spaces they print.
        let flat = doc
            .replace("\\n", " ")
            .replace('\\', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        texts.extend(
            flat.split(". ")
                .filter(|s| s.contains("go-ahead"))
                .map(str::to_string),
        );
    }
    texts
}

/// NO TEXT THE HARNESS TYPES ABOUT THE GO-AHEAD HOLDS A STOP PHRASE (2026-10-29 review 3): every
/// stop phrase stops wherever it is said, so an agent that quotes the answer, the continuation or
/// the docs would otherwise read as asking; the owner's own words hold none.
#[test]
fn no_go_ahead_text_holds_a_stop_phrase() {
    let texts = harness_texts();
    assert!(
        texts.iter().filter(|t| t.contains("go-ahead")).count() >= 8,
        "the docs' go-ahead sentences are read: {texts:#?}"
    );
    let held: Vec<String> = texts
        .iter()
        .flat_map(|text| {
            let flat = text
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            STOP_PHRASES
                .iter()
                .filter(move |p| flat.contains(*p))
                .map(move |p| format!("{p:?} in {text:?}"))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(held.is_empty(), "{}", held.join("\n"));
}

/// THE GO-AHEAD'S STOP PHRASES STILL GUARD A DESTRUCTIVE ACT: a go-ahead asked beside one is the
/// irreversible ask it was, whatever its marker, while a report of one had stops nothing.
#[test]
fn a_go_ahead_beside_a_destructive_act_still_asks_and_a_report_of_one_does_not() {
    for asks in [
        "Give me the go-ahead and I'll force-push the branch.",
        "With your go-ahead I'll delete the old releases.",
        "I'll wait for your go-ahead before force-pushing.",
        // An echo beside the ask, in its sentence or the next, echoes another go-ahead, and a
        // table row is the agent's own words (2026-10-29 review 2).
        "I need your go-ahead to delete the stale release tags; the standing go-ahead covers only \
         my own reversible work.",
        "I have the go-ahead for my branch, but deleting the old release tags still needs your \
         go-ahead.",
        "Status:\n│ delete old release tags │ waiting for your go-ahead │",
    ] {
        assert!(
            matches!(classify_said(Some(asks)), Said::Irreversible(_)),
            "{asks}: {:?}",
            classify_said(Some(asks))
        );
        let r = idle(asks, Some(3 * MIN));
        assert_eq!(
            act(&mut TurnEndState::default(), &r, t0()),
            answered_reversibly(),
            "{asks}"
        );
        assert!(
            matches!(
                at_under(&mut TurnEndState::default(), &r, t0(), &no_answers()),
                TurnEndAction::Escalate { .. }
            ),
            "{asks}"
        );
    }
    // A report of one parks nothing, so it gets no go-ahead: it reads as the base read it.
    for told in [
        "Rebuilt and pushed with your go-ahead; the suite is green.",
        "Got the go-ahead, so I deleted the stale worktree I made and rebuilt.",
    ] {
        let r = idle(told, Some(3 * MIN));
        assert!(r.go_ahead().is_none(), "{told}");
        let a = act(&mut TurnEndState::default(), &r, t0());
        assert!(!is_go_ahead(&a), "{told}: {a:?}");
    }
}

/// THE IRREVERSIBLE GUARD AS THE BASE KEPT IT (2026-10-29 review): a consent asked as `won't …
/// without your approval`, a stop beside a destructive command in a fence, and a stop phrase in a
/// fence read as they did before the go-ahead reader, so `REVERSIBLE_ANSWER` and the answers-off
/// escalation still meet them; only a wait phrase whose object is the go-ahead is the go-ahead
/// reader's.
#[test]
fn a_consent_asked_without_a_go_ahead_or_beside_a_fenced_command_keeps_the_guard() {
    let fenced = |verb: &str| {
        format!(
            "Rebuilt and tested. I need your approval to {verb}:\n\n```\ngit push --force origin \
             main\n```"
        )
    };
    let irreversible = [
        "I won't delete the old release tags without your approval.".to_string(),
        "I won't delete the old releases without your approval.".to_string(),
        "The release is built. I won't publish it without your approval.".to_string(),
        "I won't deploy without your go-ahead.".to_string(),
        "I can't force-push the branch without your go-ahead.".to_string(),
        "I'll deploy without waiting for you.".to_string(),
        fenced("push"),
        fenced("run"),
    ];
    for said in &irreversible {
        assert!(
            matches!(classify_said(Some(said)), Said::Irreversible(_)),
            "{said}: {:?}",
            classify_said(Some(said))
        );
    }
    for said in [
        "I won't merge without your sign-off.",
        "The old reply said:\n```\nI'm blocked on your review.\n```",
    ] {
        assert!(
            matches!(classify_said(Some(said)), Said::Stop(_)),
            "{said}: {:?}",
            classify_said(Some(said))
        );
    }
    for said in [
        "I won't push my branch without your go-ahead. Pushing it also deletes the stale release \
         tags on the remote."
            .to_string(),
        fenced("push"),
        fenced("run"),
    ] {
        let mut st = TurnEndState::default();
        assert_eq!(
            act(&mut st, &idle(&said, Some(3 * MIN)), t0()),
            answered_reversibly(),
            "{said}"
        );
    }
    let mut st = TurnEndState::default();
    let r = idle(&irreversible[2], Some(3 * MIN));
    assert!(
        matches!(
            at_under(&mut st, &r, t0(), &no_answers()),
            TurnEndAction::Escalate { .. }
        ),
        "the person is told"
    );
}

/// A PARK BESIDE AN OUTWARD ACT THE TURN NAMES ELSEWHERE (2026-10-29 review): a need judged on
/// its own words alone, or a park whose plan sits in the turn's earlier message, gets today's
/// act, never the owner's word "said" over a publish or a delete.
#[test]
fn a_park_beside_an_outward_act_the_turn_names_keeps_todays_act() {
    let tagged = "Tagged v1.4.0; the next push runs the crates.io publish.\nNEEDS YOU: say go so I can push.";
    let mut st = TurnEndState::default();
    let a = act(&mut st, &idle(tagged, Some(3 * MIN)), t0());
    assert!(!is_go_ahead(&a), "{a:?}");
    let planned = TurnEndReading {
        turn_said: Some(
            "Plan: delete the stale release tags on the remote, then push my branch to main. \
             Tests are green."
                .to_string(),
        ),
        ..idle("Waiting for your go-ahead to push.", Some(3 * MIN))
    };
    let mut st = TurnEndState::default();
    let a = act(&mut st, &planned, t0());
    assert!(!is_go_ahead(&a), "{a:?}");
}

/// A HOLD THE PERSON GAVE STANDS IN THE REPLIES FOR THE HOUR AFTER THEIR KEY (2026-10-29 review;
/// the judge panel's design): Cruise's continuation goes after the reply to their turn, and the
/// replies after it that park again, restating the hold or attributing it to them, get no
/// go-ahead within the hour; an attributed hold never does. Past the hour the reply that restates
/// it gets the go-ahead, which says a hold a person gave still stands: no word of theirs is read,
/// and a hold that outlasts the hour is the agent's to keep.
#[test]
fn a_hold_the_person_gave_stands_in_the_replies_after_their_turn() {
    let now = t0();
    let mut st = TurnEndState::default();
    let _ = at(&mut st, &persons_turn(), now);
    let mut then = now;
    for said in [
        "Other work is done; as you asked, the push waits until you say so.",
        "Other work is done; the push waits until you say so.",
        "You stopped the build, so it stays stopped until you say to restart it.",
    ] {
        then += 10 * MIN;
        let a = act(&mut st, &idle(said, Some(3 * MIN)), then);
        assert!(!is_go_ahead(&a), "{said}: {a:?}");
    }
    let past = now + GO_AHEAD_HELD + MIN;
    let attributed = idle(
        "As you asked, the push waits until you say so.",
        Some(3 * MIN),
    );
    let (a, _) = until_acted(&mut st, &attributed, past);
    assert!(!is_go_ahead(&a), "{a:?}");
    let restated = idle("The push still waits until you say so.", Some(3 * MIN));
    let (a, _) = until_acted(&mut st, &restated, past + 10 * MIN);
    assert!(is_go_ahead(&a), "past the hour: {a:?}");
}

/// THE TURN THE HARNESS ATTACHED TO IS NO PERSON'S: a tab already parked when the harness first
/// reads it (the incident's, two days in) gets the go-ahead at once, not after an hour's hold.
#[test]
fn a_park_the_harness_attaches_to_gets_the_go_ahead() {
    let mut st = TurnEndState::default();
    let r = TurnEndReading {
        answered: Some(ScreenAnswer {
            prompt: 7,
            work: None,
            harness: false,
        }),
        ..idle(INCIDENT, Some(2 * MIN))
    };
    let (a, _) = until_acted(&mut st, &r, t0());
    assert!(is_go_ahead(&a), "{a:?}");
    assert!(!st.go_ahead_held(t0()));
}

/// A REPLY TO A HOST'S FACT IS NO PERSON'S TURN (2026-10-29 review): the disk-free ask, an
/// upgrade's tell or Cruise's carry at a break leaves the go-ahead owed.
#[test]
fn a_reply_to_a_host_fact_or_a_carry_still_gets_the_go_ahead() {
    for carried in [false, true] {
        // An earlier turn, two hours back.
        let mut st = TurnEndState::default();
        st.observe(
            &TurnEndReading {
                answered: Some(ScreenAnswer {
                    prompt: 1,
                    work: None,
                    harness: false,
                }),
                ..idle("Fetched main; nothing moved.", Some(3 * MIN))
            },
            t0() - 2 * GO_AHEAD_HELD,
        );
        if carried {
            st.background_carried(t0() + MIN, "carry on");
        } else {
            st.fact_typed();
        }
        let r = TurnEndReading {
            answered: Some(ScreenAnswer {
                prompt: 2,
                work: None,
                harness: false,
            }),
            ..idle(INCIDENT, Some(2 * MIN))
        };
        let (a, _) = until_acted(&mut st, &r, t0() + 10 * MIN);
        assert!(is_go_ahead(&a), "carried {carried}: {a:?}");
    }
}

/// AN AGENT THAT WAITS FOR ROOM AS THE ANSWER TOLD IT IS NO REFUSAL (2026-10-29 review): it
/// drops its NEEDS YOU line as told, and its replies naming the wait for memory park nothing, so
/// no Now exception goes out over the incident's day of low disk.
#[test]
fn an_agent_that_waits_for_room_as_told_is_never_told_as_refusing() {
    use crate::supervise::policy::exception::Exception;
    let first = "NEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare (the run on \
                 the merged branch was stopped for low memory and I'm not restarting it on my own).";
    // The wait for room said after the go-ahead it waits under (review 2), before a go-ahead it
    // sets aside, and as a stop note's restart once there is room (review 5).
    for compliant in [
        "Memory is still short (13 GiB of swap), so per the go-ahead I'm waiting for room first, \
         then I'll rerun the checks.",
        "Proceeding as told. The build is waiting for memory, not for a go-ahead; the memory wait \
         runs in the background.",
        "Memory is still critical, so the build is waiting for room, not for a go-ahead.",
        "Per the stop notice, I'm not restarting the build until memory frees up; the memory wait \
         runs in the background.",
    ] {
        let mut now = t0();
        let mut st = TurnEndState::default();
        let (a, then) = until_acted(&mut st, &idle(first, Some(3 * MIN)), now);
        assert!(go_ahead_text(&a).contains(GO_AHEAD_ROOM), "{a:?}");
        now = then + 5 * MIN;
        let start = now;
        while now < start + 2 * GO_AHEAD_SPAN {
            let r = idle(compliant, Some(3 * MIN));
            let (a, then) = until_acted(&mut st, &r, now);
            assert!(
                a != TurnEndAction::Nothing && !is_go_ahead(&a),
                "{compliant} at +{:?}: {a:?}",
                then - start
            );
            assert_ne!(
                exception_for(&st, &r, &cfg(), then),
                Some(Exception::refuses_go_ahead()),
                "{compliant} at +{:?}",
                then - start
            );
            now = then + 5 * MIN;
        }
    }
}

/// A GO-AHEAD ASKED WITH NO DESTRUCTIVE ACT STILL STOPS (2026-10-29 review 2): the go-ahead's
/// stop phrases read as the base read them wherever one is asked for, marker or not, so the
/// person is told with answers off; a report, a negated need and an echo get no go-ahead.
#[test]
fn a_go_ahead_asked_with_no_destructive_act_still_stops() {
    for asks in [
        "Give me the go-ahead and I'll rotate the production API keys.",
        "On your go-ahead I'll email the customers.",
        "With your go-ahead I'll open the tracking issue.",
        "I'll push the merged branch with your go-ahead.",
    ] {
        assert!(
            matches!(classify_said(Some(asks)), Said::Stop(_)),
            "{asks}: {:?}",
            classify_said(Some(asks))
        );
        assert!(
            matches!(
                at_under(
                    &mut TurnEndState::default(),
                    &idle(asks, Some(3 * MIN)),
                    t0(),
                    &no_answers()
                ),
                TurnEndAction::Escalate { .. }
            ),
            "{asks}"
        );
    }
    // A report, a negated need and an echo park nothing: they get no go-ahead.
    for told in [
        "Rebuilt and pushed with your go-ahead; the suite is green.",
        "Rebuilt and pushed to main with your go-ahead.",
        "I rebuilt without waiting for your go-ahead, per the ruling.",
        "I no longer need your go-ahead to rebuild.",
        "This adds a go-ahead reader; its rows are in the table.",
        "The rebuild that was waiting for your go-ahead is done: built, tested and pushed to main.",
    ] {
        let a = act(
            &mut TurnEndState::default(),
            &idle(told, Some(3 * MIN)),
            t0(),
        );
        assert!(!is_go_ahead(&a), "{told}: {a:?}");
    }
}

/// A REPORT THAT NARRATES THE PARK IT CLEARED (2026-10-29 review 2): a wait in the past tense is
/// no park, so the reply gets the act it would without the go-ahead reader (its stop phrase reads
/// as the base read it, 2026-10-29 review 3), never the go-ahead, whose count would tell the
/// person an agent that finished refuses it.
#[test]
fn a_report_naming_a_past_wait_gets_no_go_ahead() {
    let said = "The rebuild that was waiting for your go-ahead is done: built, tested and pushed to \
                main.";
    let done = idle(said, Some(3 * MIN));
    assert!(done.go_ahead().is_none());
    assert!(matches!(classify_said(Some(said)), Said::Stop(_)));
    let a = act(&mut TurnEndState::default(), &done, t0());
    assert_eq!(a.rule_id(), Some(RULE_ANSWER), "{a:?}");
    let finished = idle(&format!("{said}\nATERM-TASK-DONE"), Some(3 * MIN));
    let a = act(&mut TurnEndState::default(), &finished, t0());
    assert_eq!(a.rule_id(), Some(RULE_DONE_CHECK), "{a:?}");
}

/// The PARK rows whose prose the base reads as an irreversible ask: these alone keep
/// `REVERSIBLE_ANSWER`.
const IRREVERSIBLE_PARKS: &[&str] = &[
    "No force-push and nothing published. I'm still waiting for your go-ahead before rebuilding.",
];

/// EVERY PARK ROW AT A CRUISE TURN END (2026-10-29 review): each gets the owner's standing
/// go-ahead, except the rows [`IRREVERSIBLE_PARKS`] names, whose prose is an irreversible ask and
/// keeps `REVERSIBLE_ANSWER`; a classifier change that turns another row irreversible fails here
/// (2026-10-29 review 2).
#[test]
fn every_park_row_gets_the_go_ahead_or_the_reversible_answer() {
    use super::super::go_ahead::tests::{DRAWN, Row, drawn, rendered, table};
    let mut wrong = Vec::new();
    for (row, reply) in table() {
        if row != Row::Park {
            continue;
        }
        // At every width a tab draws it (2026-10-29 review 3: a need wrapped at 100 columns), as
        // written and as Claude Code renders it, its code bare (review 5).
        for (columns, bare) in DRAWN.into_iter().flat_map(|c| [(c, false), (c, true)]) {
            let said = if bare {
                rendered(&reply)
            } else {
                reply.clone()
            };
            let r = idle(&drawn(&said, columns), Some(3 * MIN));
            let mut st = TurnEndState::default();
            let (a, _) = until_acted(&mut st, &r, t0());
            let kept = if IRREVERSIBLE_PARKS.contains(&reply.as_str()) {
                a.rule_id() == Some(RULE_ANSWER)
                    && matches!(&a, TurnEndAction::Type { text, .. } if text.starts_with(REVERSIBLE_ANSWER))
            } else {
                is_go_ahead(&a)
            };
            if !kept {
                wrong.push(format!("{reply} at {columns:?}, bare {bare}: {a:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// THE INCIDENT'S TURN AT A CRUISE TURN END, AS THE SCREEN DRAWS IT (2026-10-29 review 5): its
/// earlier messages name code Claude Code draws bare (`for_launch`, `ship/release.rs`), and the
/// whole policy answers its need with the go-ahead at every width.
#[test]
fn the_incidents_turn_drawn_bare_gets_the_go_ahead() {
    use super::super::go_ahead::tests::{DRAWN, INCIDENT_TURN, drawn, rendered};
    let (earlier, tail) = INCIDENT_TURN;
    let mut wrong = Vec::new();
    for columns in DRAWN {
        let said: Vec<String> = earlier
            .iter()
            .map(|m| drawn(&rendered(m), columns))
            .collect();
        let r = TurnEndReading {
            turn_said: Some(said.join("\n\n")),
            ..idle(&drawn(&rendered(tail), columns), Some(3 * MIN))
        };
        let (a, _) = until_acted(&mut TurnEndState::default(), &r, t0());
        if !is_go_ahead(&a) {
            wrong.push(format!("{columns:?}: {a:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The rooms the answer names are waits `aterm ctl @self await sys` takes as written: the
/// command, and the disk's predicate for it.
#[test]
fn the_room_the_answer_names_is_a_wait_aterm_takes() {
    let waits: Vec<String> = GO_AHEAD_ROOM
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|c| match c.strip_prefix("aterm ctl @self await ") {
            Some(command) => Some(command.to_string()),
            None => c.starts_with('\'').then(|| format!("sys {c}")),
        })
        .collect();
    assert_eq!(waits.len(), 2, "{GO_AHEAD_ROOM}");
    for wait in waits {
        let unquoted = wait.replace('\'', "");
        assert!(
            aterm_vitals::sys::predicate::AwaitSys::parse(&unquoted).is_ok(),
            "{wait}: {:?}",
            aterm_vitals::sys::predicate::AwaitSys::parse(&unquoted)
        );
    }
}

/// A GO-AHEAD ASKED BESIDE A PUBLISH OR A DELETION THE ANSWER'S ECHO NAMES (2026-10-29 review 3):
/// each reads as the base read it, and none gets the go-ahead; an irreversible one gets
/// `REVERSIBLE_ANSWER`, and with answers off every one is told.
#[test]
fn a_go_ahead_asked_beside_an_upload_or_quoting_the_harness_keeps_the_guard() {
    let upload = "I've built the installer and with your go-ahead I'll upload it to the App Store.";
    assert!(
        matches!(classify_said(Some(upload)), Said::Stop(_)),
        "{:?}",
        classify_said(Some(upload))
    );
    let irreversible = [
        "The [aterm harness] go-ahead covers my branch only, so deleting the old release tags still \
         needs your go-ahead.",
        "As I said, \"deleting the old release tags needs your go-ahead.\"",
        "I'll keep waiting until I have your go-ahead to delete the old release tags.",
    ];
    for said in irreversible {
        assert!(
            matches!(classify_said(Some(said)), Said::Irreversible(_)),
            "{said}: {:?}",
            classify_said(Some(said))
        );
        let r = idle(said, Some(3 * MIN));
        assert_eq!(
            act(&mut TurnEndState::default(), &r, t0()),
            answered_reversibly(),
            "{said}"
        );
    }
    for said in std::iter::once(upload).chain(irreversible) {
        let r = idle(said, Some(3 * MIN));
        assert!(
            !is_go_ahead(&act(&mut TurnEndState::default(), &r, t0())),
            "{said}"
        );
        assert!(
            matches!(
                at_under(&mut TurnEndState::default(), &r, t0(), &no_answers()),
                TurnEndAction::Escalate { .. }
            ),
            "{said}"
        );
    }
}

/// THE WORD TO SAY GOES ONLY WHERE NOTHING ELSE ASKS IT (2026-10-29 review 3): a reply whose
/// other need asks the same `go` for a deletion on the remote gets the go-ahead for its own
/// reversible need with the park quoted, never `go` said.
#[test]
fn a_go_ahead_never_says_a_word_another_need_asks_for_an_excluded_act() {
    let two = "NEEDS YOU: say go so I can rerun the checks.\nNEEDS YOU: say go so I can delete the \
               stale release tags on the remote.";
    let mut st = TurnEndState::default();
    let (a, _) = until_acted(&mut st, &idle(two, Some(3 * MIN)), t0());
    let text = go_ahead_text(&a);
    assert!(
        !text.contains(TOKEN_OPEN) && !text.contains(TOKEN_CLOSE),
        "{text}"
    );
    assert!(text.contains(GO_AHEAD_OTHERS), "{text}");
}

/// The person's turn ending where it is read, their key in it, the reply under it brief.
fn persons_turn() -> TurnEndReading {
    TurnEndReading {
        person: Some(8 * SECS),
        key: Some(8 * SECS),
        ..idle("OK, I won't push until you say so.", Some(5 * SECS))
    }
}

/// A KEY OF THE PERSON'S IN A TURN THE HARNESS OPENED HOLDS THE GO-AHEAD FOR THE HOUR (2026-10-29
/// reviews 3 and 4, read by keys): a message they queued while an act of the harness's, a host's
/// fact or a carry ran, or any key of theirs in that turn, came after the harness's line, so the
/// hour runs from the turn's end, whatever words open it on screen; past the hour it holds
/// nothing.
#[test]
fn a_key_in_a_turn_the_harness_opened_holds_the_hour() {
    for opener in ["act", "fact", "carry"] {
        let mut st = TurnEndState::default();
        let start = if opener == "act" {
            let committed = idle(
                "Fixed the parser and committed it on my branch.",
                Some(3 * MIN),
            );
            let (a, acted) = until_acted(&mut st, &committed, t0());
            assert!(matches!(a, TurnEndAction::Type { .. }), "{a:?}");
            acted
        } else {
            st.observe(
                &TurnEndReading {
                    answered: Some(ScreenAnswer {
                        prompt: 1,
                        work: None,
                        harness: false,
                    }),
                    ..idle("Fetched main; nothing moved.", Some(3 * MIN))
                },
                t0() - 2 * GO_AHEAD_HELD,
            );
            if opener == "carry" {
                st.background_carried(
                    t0(),
                    crate::supervise::policy::cruise_carry::BACKGROUND_CARRY,
                );
            } else {
                st.fact_typed();
            }
            t0()
        };
        let ended = start + 4 * MIN;
        let theirs = TurnEndReading {
            person: Some(90 * SECS),
            key: Some(90 * SECS),
            answered: Some(ScreenAnswer {
                prompt: 9,
                work: None,
                harness: false,
            }),
            ..idle("The push still waits until you say so.", Some(2 * MIN))
        };
        st.observe(&theirs, ended);
        assert!(st.go_ahead_held(ended + GO_AHEAD_HELD - MIN), "{opener}");
        assert!(!st.go_ahead_held(ended + GO_AHEAD_HELD), "{opener}");
    }
}

/// A reading's strength as the guard acts on it (`asked_by`): an irreversible ask over a stop over
/// a question over nothing asked. An offer is nothing asked, as plain prose is: a question the
/// base read that today reads as an offer has lost its ask (2026-10-29 merge review).
fn guard_strength(said: &Said) -> u8 {
    match said {
        Said::Irreversible(_) => 3,
        Said::Stop(_) => 2,
        Said::Question => 1,
        Said::Offer | Said::Plain => 0,
    }
}

/// The base's guard (246d1ddc2f) for each text of the corpus, as `fixtures/go-ahead-guard-base.txt`
/// holds it (`guard_base_tests.rs`): its cut, and the kind of its reading.
fn guard_base() -> std::collections::HashMap<String, (String, Said)> {
    use crate::supervise::policy::guard_base_tests::{SAME, unescaped};
    include_str!("fixtures/go-ahead-guard-base.txt")
        .lines()
        .filter(|l| l.contains('\t'))
        .map(|line| {
            let mut fields = line.split('\t');
            let mut field = || fields.next().expect("three fields");
            let text = unescaped(field());
            let cut = match field() {
                SAME => text.clone(),
                cut => unescaped(cut),
            };
            let reading = unescaped(field());
            let said = match reading.split_once(':') {
                Some(("stop", why)) => Said::Stop(why.to_string()),
                Some(("irreversible", why)) => Said::Irreversible(why.to_string()),
                _ => match reading.as_str() {
                    "plain" => Said::Plain,
                    "offer" => Said::Offer,
                    "question" => Said::Question,
                    other => panic!("a reading: {other}"),
                },
            };
            (text, (cut, said))
        })
        .collect()
}

/// THE GUARD READS EVERY REPLY AT LEAST AS THE BASE DID (2026-10-29 reviews 4, 5 and 6, and the
/// merge with main's `needs::apart`): the guard's reading of each text is at least as strong as
/// the one 246d1ddc2f made of it (an irreversible ask over a stop over a question over nothing
/// asked, an offer or plain prose), for every table row at every width a tab draws it, every review repro
/// and every echo: `fixtures/go-ahead-guard-base.txt` holds the base's own output, written at
/// 246d1ddc2f (`guard_base_tests.rs`), so a weaker reading shows here whatever today's code makes
/// of it. What the guard reads ([`TurnEndReading::asked`]) is `classify_said` of
/// `needs::without_needs`, the cut whose walk takes no go-ahead join
/// (`needs::tests::the_guards_cut_takes_no_go_ahead_join`), not the base's bytes: the cut may
/// change with the needs reader, the reading may not weaken. Where the guard reads an
/// irreversible ask the reply gets `REVERSIBLE_ANSWER`, or the go-ahead only where the ask rests
/// on the agent's own cleanup on rows a parking need took ([`go_ahead_answers`]), and where it
/// reads any ask (a stop, a question, an irreversible ask) answers off tell it.
#[test]
fn the_guard_reads_every_reply_as_the_base_did() {
    use super::super::go_ahead::tests::guard_corpus;
    let base = guard_base();
    let texts = guard_corpus();
    let mut wrong = Vec::new();
    let (mut irreversible, mut asks, mut cut) = (0, 0, 0);
    for text in &texts {
        let Some((base_cut, base_said)) = base.get(text) else {
            wrong.push(format!(
                "no base record (rerun guard_base_tests.rs at 246d1ddc2f): {text:?}"
            ));
            continue;
        };
        let guard = needs::without_needs(text);
        cut += usize::from(base_cut != text);
        let read = classify_said(Some(&guard));
        if guard_strength(&read) < guard_strength(base_said) {
            wrong.push(format!(
                "{read:?} weaker than the base's {base_said:?}: {text:?}, cut {guard:?}, the \
                 base's {base_cut:?}"
            ));
        }
        let r = idle(text, Some(3 * MIN));
        let fed = match &read {
            Said::Irreversible(why) => Some((why.clone(), true)),
            Said::Stop(why) => Some((why.clone(), false)),
            Said::Question => Some(("the worker asked a question".to_string(), false)),
            Said::Offer | Said::Plain => None,
        };
        if r.asked() != fed {
            wrong.push(format!(
                "the guard read other words: {:?}: {text:?}",
                r.asked()
            ));
        }
        if fed.is_none() {
            continue;
        }
        asks += 1;
        if !matches!(
            at_under(&mut TurnEndState::default(), &r, t0(), &no_answers()),
            TurnEndAction::Escalate { .. }
        ) {
            wrong.push(format!("not told with answers off: {text:?}"));
        }
        if !matches!(read, Said::Irreversible(_)) {
            continue;
        }
        irreversible += 1;
        let a = act(&mut TurnEndState::default(), &r, t0());
        let kept = answers_reversibly(&a) || (is_go_ahead(&a) && go_ahead_answers(Some(text)));
        if !kept {
            wrong.push(format!("{a:?}: {text:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // The controls: the corpus holds texts the base cut, asks, irreversible asks, and every kind
    // of the base's reading.
    assert!(
        cut > 0 && asks > irreversible && irreversible > 0,
        "{cut} {asks} {irreversible}"
    );
    for kind in [0, 1, 2, 3] {
        assert!(
            base.values().any(|(_, said)| guard_strength(said) == kind),
            "no base reading of strength {kind}"
        );
    }
}

/// A NEED WRAPPED MID-CLAUSE OR PAST A QUOTE OVER A DELETION ON THE REMOTE KEEPS THE GUARD
/// (2026-10-29 review 4): at 100 columns the row after the need's `so`, or the row that opens with
/// the quoted word, is the need's only in the go-ahead reader's view; the guard reads it as the
/// reply's own, and the reply gets `REVERSIBLE_ANSWER`, or with answers off is told.
#[test]
fn a_need_wrapped_over_a_remote_deletion_keeps_the_guard() {
    use super::super::go_ahead::tests::drawn;
    for said in [
        "The rebuilt branch is waiting for your go-ahead.\n\nNEEDS YOU: rerun the checks on the \
         merged upgrade fix branch and its tests first, then say \"go\" so I can delete the stale \
         release tags on the remote.",
        "The rebuilt branch is waiting for your go-ahead.\n\nNEEDS YOU: rerun the checks on the \
         merged upgrade fix branch and its parser fix first, then say \"go\" to delete the stale \
         release tags on the remote.",
    ] {
        let text = drawn(said, Some(100));
        assert!(
            !needs::go_ahead_view(&text).moved.is_empty(),
            "the view moves the wrapped row: {text}"
        );
        let r = idle(&text, Some(3 * MIN));
        let a = act(&mut TurnEndState::default(), &r, t0());
        assert!(answers_reversibly(&a), "{text}: {a:?}");
        assert!(
            matches!(
                at_under(&mut TurnEndState::default(), &r, t0(), &no_answers()),
                TurnEndAction::Escalate { .. }
            ),
            "{text}"
        );
    }
}

/// A NEED WRAPPED MID-CLAUSE OVER AN IRREVERSIBLE ACT KEEPS THE GUARD (2026-10-29 reviews 3 and
/// 4): the go-ahead reader's need takes its wrapped row, but the guard reads the reply as the
/// base did, that row as the reply's own words, and the go-ahead answers no irreversible ask but
/// the agent's own cleanup: the reply gets `REVERSIBLE_ANSWER`, never the go-ahead its prose
/// would otherwise get.
#[test]
fn a_need_wrapped_over_a_force_push_keeps_the_guard() {
    let said = "The branch is rebuilt and waiting for your go-ahead.\n\nNEEDS YOU: say \"go\" so\nI \
                can force-push main over the old history.";
    assert_eq!(
        needs::needs_you(said),
        ["say \"go\" so"],
        "the base's needs"
    );
    let r = idle(said, Some(3 * MIN));
    let a = act(&mut TurnEndState::default(), &r, t0());
    assert!(
        a.rule_id() == Some(RULE_ANSWER)
            && matches!(&a, TurnEndAction::Type { text, .. } if text.starts_with(REVERSIBLE_ANSWER)),
        "{a:?}"
    );
    assert!(matches!(
        at_under(&mut TurnEndState::default(), &r, t0(), &no_answers()),
        TurnEndAction::Escalate { .. }
    ));
}

/// A `NEEDS YOU items …` ROW UNDER A CHOICE OR A QUESTION IS CUT AS THE BASE CUT IT (2026-10-29
/// review 5): the guard reads a choice or a question from the reply's last unit, so a row the
/// base cut as a need and the guard read as prose would turn an irreversible choice, or a
/// question, into nothing asked.
#[test]
fn a_needs_you_items_row_under_a_choice_or_a_question_keeps_the_guard() {
    let choice = "Two ways to clear the stale tags:\n1. Delete the stale release tags and re-tag.\n2. \
                  Leave them in place.\nWhich one?\nNEEDS YOU items stay on the menu bar until you \
                  answer.";
    let question = "Should I rebase onto main first?\nNEEDS YOU items stay on the menu bar until \
                    you answer.";
    let r = idle(choice, Some(3 * MIN));
    let a = act(&mut TurnEndState::default(), &r, t0());
    assert!(answers_reversibly(&a), "{a:?}");
    for said in [choice, question] {
        let r = idle(said, Some(3 * MIN));
        assert!(
            matches!(
                at_under(&mut TurnEndState::default(), &r, t0(), &no_answers()),
                TurnEndAction::Escalate { .. }
            ),
            "{said}"
        );
    }
}

/// A KEY OF THE PERSON'S IN A TURN THAT ENDS AT A WALL HOLDS THE HOUR (2026-10-29 review 5, read
/// by keys): the key is read wherever a point is judged, so an overload, an API error, a usage
/// limit, a full context or the memory banner holds the go-ahead for the hour from the turn's end
/// and no longer; the reply after the harness's retry that restates a hold gets no go-ahead
/// within it.
#[test]
fn a_persons_turn_ending_at_a_wall_holds_the_hour() {
    for kind in [
        WallKind::Overloaded,
        WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: aterm_phase::ApiCause::Server,
        },
        WallKind::UsageSession,
        WallKind::Context,
        WallKind::Memory,
    ] {
        let mut st = TurnEndState::default();
        let theirs = TurnEndReading {
            wall: Some(kind),
            ..persons_turn()
        };
        st.observe(&theirs, t0());
        assert!(st.go_ahead_held(t0() + GO_AHEAD_HELD - MIN), "{kind:?}");
        assert!(!st.go_ahead_held(t0() + GO_AHEAD_HELD), "{kind:?}");
    }
    let mut st = TurnEndState::default();
    let theirs = TurnEndReading {
        wall: Some(WallKind::Overloaded),
        answered: Some(ScreenAnswer {
            prompt: 3,
            work: None,
            harness: false,
        }),
        ..persons_turn()
    };
    st.observe(&theirs, t0());
    let gone = TurnEndReading {
        person: None,
        key: None,
        worked: None,
        ..theirs
    };
    let (retry, then) = until_acted(&mut st, &gone, t0() + MIN);
    assert!(
        matches!(retry, TurnEndAction::Type { .. }),
        "the wall's retry: {retry:?}"
    );
    let restated = TurnEndReading {
        answered: Some(ScreenAnswer {
            prompt: 4,
            work: None,
            harness: false,
        }),
        ..idle(
            "I'll merge main into my branch and push it once I get your go-ahead.",
            Some(2 * MIN),
        )
    };
    let (a, _) = until_acted(&mut st, &restated, then + 5 * MIN);
    assert!(!is_go_ahead(&a), "{a:?}");
}

/// THE HARNESS'S OWN WORDS NEVER HOLD THE GO-AHEAD (2026-10-29 review 5): a go-ahead the carry
/// typed at a break, whose turn passed a second break before it ended at a point (the fact of it
/// dropped there), with no key of the person's, holds nothing.
#[test]
fn a_turn_the_harness_opened_is_no_persons_past_a_second_break() {
    let first = "NEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare (the run on \
                 the merged branch was stopped for low memory and I'm not restarting it on my own).";
    let mut st = TurnEndState::default();
    st.observe(
        &TurnEndReading {
            answered: Some(ScreenAnswer {
                prompt: 1,
                work: None,
                harness: false,
            }),
            ..idle("Fetched main; nothing moved.", Some(3 * MIN))
        },
        t0() - 2 * GO_AHEAD_HELD,
    );
    let typed = go_ahead_answer(
        &super::super::go_ahead::parked("", first).expect("parks"),
        false,
        None,
    );
    assert!(typed.contains(GO_AHEAD_ROOM));
    st.background_carried(t0(), &typed);
    st.took();
    st.fact_answered();
    let r = TurnEndReading {
        answered: Some(ScreenAnswer {
            prompt: 2,
            work: None,
            harness: false,
        }),
        ..idle(first, Some(2 * MIN))
    };
    let (a, then) = until_acted(&mut st, &r, t0() + 10 * MIN);
    assert!(is_go_ahead(&a), "{a:?}");
    assert!(!st.go_ahead_held(then));
}

/// A KEY OF THE PERSON'S AT A LOOP'S FIRST POINT HOLDS FROM THE KEY (2026-10-29 review 6, read by
/// keys): an idle attach, a lift off Off and a restart whose books end on a typed act read the key
/// the window measured five minutes back, no work measured, and hold the go-ahead for the rest of
/// that key's hour, not an hour from the point.
#[test]
fn a_key_read_at_a_first_point_with_no_work_holds_from_the_key() {
    let said = "Understood: the branch is committed, and I'll push it once you say so.";
    let r = |worked| TurnEndReading {
        answered: Some(ScreenAnswer {
            prompt: 7,
            work: None,
            harness: false,
        }),
        person: Some(5 * MIN),
        key: Some(5 * MIN),
        ..idle(said, worked)
    };
    for first in ["attach", "lifted", "seeded"] {
        let mut st = TurnEndState::default();
        match first {
            "lifted" => st.start_lifted_off(OffStood::Person),
            "seeded" => st.seed_yield(RULE_CONTINUE),
            _ => {}
        }
        let worked = (first == "seeded").then_some(2 * MIN);
        let a = at(&mut st, &r(worked), t0());
        assert!(!is_go_ahead(&a), "{first}: {a:?}");
        assert!(st.go_ahead_held(t0() + GO_AHEAD_HELD - 6 * MIN), "{first}");
        assert!(!st.go_ahead_held(t0() + GO_AHEAD_HELD - 5 * MIN), "{first}");
    }
}

/// THE HOUR'S END DECIDES A HELD PARK AGAIN (2026-10-30 review 8): the person's key holds a park
/// for the hour, Cruise's continuation answers it until the same reply holds, and that hold waits
/// for the hour's end instead of standing with nothing owed. The loop decides the unchanged point
/// again there and the go-ahead goes, though the parked agent ends no other turn.
#[test]
fn a_park_the_hour_holds_is_decided_again_at_its_end() {
    let start = t0();
    let key = start - 8 * SECS;
    let read = |worked: Option<Duration>, now: Instant| TurnEndReading {
        person: Some(now.duration_since(key)),
        key: Some(now.duration_since(key)),
        ..idle(INCIDENT, worked)
    };
    let hour = start + GO_AHEAD_HELD;
    // Each wait inside the hour (the typing guard, a back-off) waited out on the same point, as
    // the loop's due does.
    let step = |st: &mut TurnEndState, worked: Option<Duration>, mut now: Instant| {
        let mut a = act(st, &read(worked, now), now);
        while let TurnEndAction::WaitUntil { until, .. } = a
            && until < hour
        {
            now = until;
            a = act(st, &read(None, now), now);
        }
        (a, now)
    };
    let mut st = TurnEndState::default();
    let (mut a, mut now) = step(&mut st, Some(5 * MIN), start);
    let mut answers = 0;
    while matches!(a, TurnEndAction::Type { .. }) {
        assert!(!is_go_ahead(&a), "within the hour: {a:?}");
        assert!(now < hour, "never held");
        answers += 1;
        (a, now) = step(&mut st, Some(5 * SECS), now + 5 * SECS);
    }
    assert!(answers >= 2 && st.repeats(&read(None, now)), "{a:?}");
    assert!(
        matches!(a, TurnEndAction::WaitUntil { until, .. } if until == hour),
        "the hold waits for the hour's end: {a:?}"
    );
    let a = act(&mut st, &read(None, hour), hour);
    assert!(is_go_ahead(&a), "{a:?}");
}

/// The incident's park reworded, as its agent wrote each one: the same word to say, other words
/// around it, so no same reply's hold forms.
const REWORDED: &str = "No change since the last check: the build still waits for your \
                        go-ahead.\n\nNEEDS YOU: say \"rerun the checks\" and I'll build and test \
                        the upgrade fix branch, push it to main and remove the worktree.";

/// A BACK-OFF NEVER OUTLASTS THE PERSON'S HOUR (2026-10-30 review 9): the incident's tab, a day
/// of short turns behind it, backs off for the ladder's top, an hour. The person's key comes in
/// a turn that ends on a park, so their hour holds it; half an hour on the agent's own wake ends
/// a new point on a reworded park, whose back-off, counted from that point, would run half an
/// hour past the hour's end. It ends at the hour's end instead, where the park is owed and the
/// go-ahead goes, nobody typing.
#[test]
fn a_back_off_inside_the_hour_ends_at_the_hours_end() {
    let mut st = TurnEndState::default();
    let mut now = t0();
    for i in 0..8 {
        let r = idle(
            &format!("Checked the queue ({i}): the build still waits its turn."),
            Some(5 * SECS),
        );
        (_, now) = until_acted(&mut st, &r, now + 5 * SECS);
    }
    let top = TurnEndTiming::default().short_backoff_max;
    assert_eq!(st.short_wait(), Some(top), "the ladder's top");
    let p1 = now + MIN;
    let key = p1 - 8 * SECS;
    let read = |said: &str, worked: Option<Duration>, at: Instant| TurnEndReading {
        person: Some(at.duration_since(key)),
        key: Some(at.duration_since(key)),
        ..idle(said, worked)
    };
    let hour = p1 + GO_AHEAD_HELD;
    let mut a = act(&mut st, &read(INCIDENT, Some(5 * SECS), p1), p1);
    while let TurnEndAction::WaitUntil { until, .. } = a
        && until < p1 + 30 * MIN
    {
        a = act(&mut st, &read(INCIDENT, None, until), until);
    }
    assert!(!is_go_ahead(&a), "within the hour: {a:?}");
    let p2 = p1 + 30 * MIN;
    let a = act(&mut st, &read(REWORDED, Some(5 * SECS), p2), p2);
    let TurnEndAction::WaitUntil { until, .. } = a else {
        panic!("the hour holds the reworded park: {a:?}");
    };
    assert!(
        until <= hour,
        "the back-off ends at the hour's end, not {:?} past it",
        until.duration_since(hour)
    );
    let a = act(&mut st, &read(REWORDED, None, until), until);
    assert!(is_go_ahead(&a), "{a:?}");
}

/// A PARK THE HOUR HOLDS BEFORE LAND'S INSTRUCTION GETS IT AT THE HOUR'S END (2026-10-30 review
/// 9): the session worked under Cruise, the chip went to Land, and the person's key comes in a
/// turn that ends on the incident's park, Land's instruction untyped. Land answers it within the
/// hour until the same reply holds; that hold waits for the hour's end, tells the person nothing
/// to do, and lets the point through there: Land's instruction goes first, then the go-ahead at
/// the same reply, nobody typing.
#[test]
fn a_park_the_hour_holds_before_lands_instruction_gets_it_at_the_hours_end() {
    let mut st = TurnEndState::default();
    let (_, worked) = until_acted(
        &mut st,
        &idle("Committed the parser fix on my branch.", Some(5 * MIN)),
        t0(),
    );
    let p1 = worked + 10 * MIN;
    let key = p1 - 8 * SECS;
    let read = |worked: Option<Duration>, at: Instant| {
        land(TurnEndReading {
            person: Some(at.duration_since(key)),
            key: Some(at.duration_since(key)),
            ..idle(INCIDENT, worked)
        })
    };
    let hour = p1 + GO_AHEAD_HELD;
    let step = |st: &mut TurnEndState, worked: Option<Duration>, mut now: Instant| {
        let mut a = act(st, &read(worked, now), now);
        while let TurnEndAction::WaitUntil { until, .. } = a
            && until < hour
        {
            now = until;
            a = act(st, &read(None, now), now);
        }
        (a, now)
    };
    let (mut a, mut now) = step(&mut st, Some(5 * SECS), p1);
    let mut answers = 0;
    while matches!(a, TurnEndAction::Type { .. }) {
        assert!(
            !is_go_ahead(&a) && a.rule_id() != Some(RULE_LAND),
            "within the hour: {a:?}"
        );
        assert!(now < hour, "never held");
        answers += 1;
        (a, now) = step(&mut st, Some(5 * SECS), now + 5 * SECS);
    }
    assert!(
        answers >= 1 && st.repeats(&read(None, now)),
        "{answers} answers: {a:?}"
    );
    assert!(
        matches!(a, TurnEndAction::WaitUntil { until, .. } if until == hour),
        "the hold waits for the hour's end: {a:?}"
    );
    let told = exception_for(&st, &read(None, now), &cfg(), now);
    assert!(told.is_none(), "nothing for the person to do: {told:?}");
    let a = act(&mut st, &read(None, hour), hour);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND),
        "Land's instruction first: {a:?}"
    );
    let (a, _) = until_acted_keyed(&mut st, &read(Some(5 * SECS), hour + MIN), hour + MIN);
    assert!(is_go_ahead(&a), "then the go-ahead: {a:?}");
}

/// THE PERSON'S HOUR COUNTS THE MAC'S SLEEP (2026-10-30 review 9): the person types as a turn
/// opens, the turn ends on the incident's park, and their hour holds it. The lid closes a minute
/// on and opens two hours later, where `Instant` stood still: the loop says the Mac slept
/// ([`TurnEndState::slept`]), and the window's typing age counts the sleep. The park the agent
/// ends at the wake gets the go-ahead then, not at the hour's end in awake time.
#[test]
fn the_persons_hour_counts_the_macs_sleep() {
    let mut st = TurnEndState::default();
    let p1 = t0();
    let asleep = 2 * 60 * MIN;
    let key = p1 - 3 * MIN;
    // The person's key on the awake clock; once the Mac slept, its age counts the sleep.
    let read = |worked: Option<Duration>, at: Instant, slept: Duration| TurnEndReading {
        person: Some(at.duration_since(key) + slept),
        key: Some(at.duration_since(key) + slept),
        ..idle(INCIDENT, worked)
    };
    let a = act(&mut st, &read(Some(3 * MIN), p1, Duration::ZERO), p1);
    assert!(!is_go_ahead(&a), "within the hour: {a:?}");
    assert!(st.go_ahead_held(p1 + MIN), "the hour holds");
    st.slept(asleep);
    let woke = p1 + 2 * MIN;
    let (a, at) = until_acted_keyed(&mut st, &read(Some(MIN), woke, asleep), woke);
    assert!(is_go_ahead(&a), "{a:?}");
    assert!(
        at < p1 + GO_AHEAD_HELD,
        "at the wake, not {:?} on in awake time",
        at.duration_since(woke)
    );
}

/// LAND'S GO-AHEAD RIDES A PARK ON LANDING WORK BY ITS OWN WORDS (2026-10-29 review 6): the
/// incident's work lived in a worktree the session's folder does not show, so a park whose need
/// names a landing act (merge, commit, push, landing) or whose turn names its branch or main is
/// unlanded work; over a clean folder each of the incident's own turns gets the go-ahead after
/// Land's instruction, at every width, as written and rendered. A park that names none of them
/// keeps Land's own act.
#[test]
fn land_gives_the_go_ahead_to_a_park_on_landing_work_over_a_clean_folder() {
    use super::super::go_ahead::tests::{DRAWN, drawn, rendered};
    let tails = [
        "The upgrade fix branch hasn't changed and is now 739 commits behind main, but a merge \
         would still conflict in only one file. NEEDS YOU: say \"rerun the checks\" so I can \
         merge, build, test and push it.",
        "Nothing more can be done without a build.\n\nNEEDS YOU: say \"rerun the checks\" when \
         the Mac has memory to spare (another tab's build is filling it), and I'll build, test, \
         commit and push this.",
        "I've started a read-only review of the unbuilt round-2 code.\n\nNEEDS YOU: say \"rerun \
         the checks\" when the Mac has memory to spare (13 GiB of swap at the last read). The two \
         builds stopped for low memory stay stopped until then, and landing waits on that check.",
        "Nothing moved this tick: memory pressure on the Mac is critical (8.9 GiB of swap), so I \
         started no builds. The round-2 fixes are still uncommitted in \
         `~/Developer/aterm-quiet-tell` and haven't been built since the last edit.\n\nMain is 26 \
         commits ahead of the branch. Two files will need merging: `CHANGELOG.md` and \
         `crates/aterm-agent/src/harness/relaunch_tests.rs`. I'll do that merge once the fixes are \
         committed.\n\nNEEDS YOU: say \"rerun the checks\" when memory frees up. The two builds \
         were stopped for low memory, and I'm not restarting them on my own.",
    ];
    let mut wrong = Vec::new();
    for tail in tails {
        for (columns, bare) in DRAWN.into_iter().flat_map(|c| [(c, false), (c, true)]) {
            let said = if bare {
                rendered(tail)
            } else {
                tail.to_string()
            };
            let parked = land(idle(&drawn(&said, columns), Some(3 * MIN)));
            let mut st = TurnEndState::default();
            let first = act(&mut st, &parked, t0());
            assert_eq!(first.rule_id(), Some(RULE_LAND), "{first:?}");
            let (a, _) = until_acted(&mut st, &parked, t0() + 10 * MIN);
            if !is_go_ahead(&a) {
                wrong.push(format!("{columns:?} bare {bare}: {a:?}: {tail}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    let bare = land(idle(
        "Nothing has changed. The build is waiting only for your go-ahead.",
        Some(3 * MIN),
    ));
    let mut st = TurnEndState::default();
    let _ = act(&mut st, &bare, t0());
    let (a, _) = until_acted(&mut st, &bare, t0() + 10 * MIN);
    assert!(!is_go_ahead(&a), "nothing unlanded: Land's own act: {a:?}");
}

/// [`until_acted`] with the person's last key where it came: each later read times it from its
/// own clock, as the window's `human_ms` does, so the wait's reads are no new key of theirs.
fn until_acted_keyed(
    st: &mut TurnEndState,
    r: &TurnEndReading,
    mut now: Instant,
) -> (TurnEndAction, Instant) {
    let came = |ago: Option<Duration>| ago.and_then(|ago| now.checked_sub(ago));
    let (person, key) = (came(r.person), came(r.key));
    let mut a = act(st, r, now);
    for _ in 0..10 {
        let TurnEndAction::WaitUntil { until, .. } = a else {
            break;
        };
        now = until;
        let since = |at: Option<Instant>| at.map(|at| now.saturating_duration_since(at));
        let same = TurnEndReading {
            worked: None,
            person: since(person),
            key: since(key),
            ..r.clone()
        };
        a = act(st, &same, now);
    }
    (a, now)
}

/// A Claude Code screen at the end of a turn: the prompt row `opened` (none where `None`) and the
/// reply `reply` under it, each line after its first in the reply's gutter, read as the loop
/// reads a turn end, `worked` its work and `person` how long before it the person's last key
/// came. The person's words stand on the screen, where any reader of them finds them.
fn screened(
    opened: Option<&str>,
    reply: &str,
    worked: Duration,
    person: Option<Duration>,
) -> TurnEndReading {
    let rule = "─".repeat(80);
    let mut rows: Vec<String> = Vec::new();
    if let Some(words) = opened {
        rows.extend([format!("❯ {words}"), String::new()]);
    }
    rows.extend(reply.lines().enumerate().map(|(i, line)| match (i, line) {
        (0, _) => format!("⏺ {line}"),
        (_, "") => String::new(),
        _ => format!("  {line}"),
    }));
    rows.extend([
        String::new(),
        rule.clone(),
        "❯".to_string(),
        rule,
        "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string(),
    ]);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    TurnEndReading {
        person,
        key: person,
        ..TurnEndReading::of(&read, &rows, false, Some(worked), None, None)
    }
}

/// The incident's park, read `after` past the turn end before it, the person's last key
/// `person` before that turn end where they typed.
fn park_after(after: Duration, person: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        person: person.map(|p| p + after),
        key: person.map(|p| p + after),
        ..idle(INCIDENT, Some(3 * MIN))
    }
}

/// THE HOUR AFTER THE PERSON'S KEY IS THE GO-AHEAD'S ONLY HOLD (owner, 2026-10-29: "how dare you
/// block on me for go ahead!"; the judge panel's design): whatever the message that opened their
/// turn says (a hold, a grant a word reader took for a hold, `Wait for the build to finish, then
/// push.` or `Not now — just land it.`, a task, the harness's own line, or nothing on screen),
/// the park after it gets the same act and no go-ahead within [`GO_AHEAD_HELD`] of the turn
/// their key ended, and the go-ahead past it. The control: the same turns with no key of theirs
/// get the go-ahead a minute on, so the hour is the key's and not the words'.
#[test]
fn every_persons_turn_holds_the_go_ahead_for_the_hour_whatever_it_says() {
    let now = t0();
    let reply = "Committed the parser fix on my branch.";
    let mut wrong = Vec::new();
    let mut within = Vec::new();
    for opened in [
        Some("Commit it, but don't push until I say so."),
        Some("Wait for the build to finish, then push."),
        Some("Not now — just land it."),
        Some("Fix the parser's error recovery."),
        Some(crate::supervise::config::CONTINUE_TEXT),
        None,
    ] {
        let key = Some(8 * SECS);
        let mut st = TurnEndState::default();
        let _ = act(&mut st, &screened(opened, reply, 5 * SECS, key), now);
        let (held, then) =
            until_acted_keyed(&mut st.clone(), &park_after(55 * MIN, key), now + 55 * MIN);
        if is_go_ahead(&held) || then >= now + GO_AHEAD_HELD {
            wrong.push(format!("{opened:?}: {held:?} within the hour"));
        }
        within.push((opened, held));
        let (a, _) = until_acted_keyed(&mut st, &park_after(61 * MIN, key), now + 61 * MIN);
        if !is_go_ahead(&a) {
            wrong.push(format!("{opened:?}: no go-ahead past the hour: {a:?}"));
        }
        let mut keyless = TurnEndState::default();
        let _ = act(&mut keyless, &screened(opened, reply, 5 * SECS, None), now);
        let (a, _) = until_acted_keyed(&mut keyless, &park_after(MIN, None), now + MIN);
        if !is_go_ahead(&a) {
            wrong.push(format!("{opened:?} with no key: {a:?}"));
        }
    }
    for pair in within.windows(2) {
        if pair[0].1 != pair[1].1 {
            wrong.push(format!("{pair:?}: two acts within the hour"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A LONG TURN THE PERSON'S KEY OPENED HOLDS FROM ITS END: their message opens a turn that runs
/// an hour and a half and ends restating the hold it gave; the hour runs from that end, so that
/// reply gets no go-ahead, and the park an hour past it does: their message held nothing longer.
#[test]
fn a_long_turn_a_persons_key_opened_holds_from_its_end() {
    let ended = t0() + 90 * MIN;
    let mut st = TurnEndState::default();
    let turn = screened(
        Some("Commit it, but don't push until I say so."),
        "Committed on my branch; the push waits until you say so.",
        90 * MIN,
        Some(90 * MIN),
    );
    let a = act(&mut st, &turn, ended);
    assert!(!is_go_ahead(&a), "{a:?}");
    let (a, _) = until_acted(
        &mut st,
        &park_after(61 * MIN, Some(90 * MIN)),
        ended + 61 * MIN,
    );
    assert!(is_go_ahead(&a), "{a:?}");
}

/// A CONTROL WRITE NEVER HOLDS THE GO-AHEAD: the harness's own act opens a turn whose reply
/// parks, with no key of the person's (the window stamps their key only for their own hand,
/// never for a control write: `aterm-gui`'s
/// `a_window_keystroke_stamps_human_ms_and_a_control_write_never_does`), and the go-ahead goes
/// at once. The control: the same turn with a key of theirs in it gets none, nor does the park
/// ten minutes on.
#[test]
fn a_control_write_never_holds_the_go_ahead() {
    let parked = "The build is waiting only for your go-ahead to rerun the checks.";
    for keyed in [false, true] {
        let mut st = TurnEndState::default();
        let (a, acted) = until_acted(
            &mut st,
            &idle(
                "Fixed the parser and committed it on my branch.",
                Some(3 * MIN),
            ),
            t0(),
        );
        let TurnEndAction::Type { text, .. } = &a else {
            panic!("an act: {a:?}")
        };
        let turn = screened(Some(text), parked, 2 * MIN, keyed.then_some(90 * SECS));
        let (a, then) = until_acted_keyed(&mut st, &turn, acted + 4 * MIN);
        assert_eq!(is_go_ahead(&a), !keyed, "keyed {keyed}: {a:?}");
        // The park ten minutes on, no key since: the key's hour still holds it.
        let key = acted + 4 * MIN - 90 * SECS;
        let again = TurnEndReading {
            person: keyed.then(|| (then + 10 * MIN).duration_since(key)),
            key: keyed.then(|| (then + 10 * MIN).duration_since(key)),
            ..idle(parked, Some(3 * MIN))
        };
        let (a, _) = until_acted_keyed(&mut st, &again, then + 10 * MIN);
        assert_eq!(
            is_go_ahead(&a),
            !keyed,
            "keyed {keyed}, ten minutes on: {a:?}"
        );
    }
}

/// A KEY BEFORE A RESTART HOLDS FROM THE WINDOW'S CLOCK: the window keeps the person's last key
/// (`human_ms`) across the supervisor's restart, so a fresh state whose first point reads a key
/// ten minutes back holds the go-ahead for the rest of that key's hour, with no books. The
/// control: a key two days back holds nothing, and the park gets the go-ahead at once, as one
/// with no key does (`a_park_the_harness_attaches_to_gets_the_go_ahead`).
#[test]
fn a_key_after_a_restart_holds_from_the_terminals_clock() {
    let attach = |person: Duration| TurnEndReading {
        answered: Some(ScreenAnswer {
            prompt: 7,
            work: None,
            harness: false,
        }),
        person: Some(person),
        key: Some(person),
        ..idle(INCIDENT, Some(2 * MIN))
    };
    let mut st = TurnEndState::default();
    let a = at(&mut st, &attach(10 * MIN), t0());
    assert!(!is_go_ahead(&a), "{a:?}");
    assert!(st.go_ahead_held(t0() + 45 * MIN));
    assert!(!st.go_ahead_held(t0() + 55 * MIN));
    let mut st = TurnEndState::default();
    let (a, _) = until_acted_keyed(&mut st, &attach(2 * 24 * 60 * MIN), t0());
    assert!(is_go_ahead(&a), "{a:?}");
}

/// A BREAK HOLDS A KEY FROM ITS OWN TIME WHERE IT CAME BEFORE THE BREAK'S TURN (2026-10-30 review
/// 8): after a restart the books seed an act 50 minutes back, and the person's key came 40
/// minutes back, before the turn the first settled break ends. The hour runs from the key, so it
/// ends 20 minutes on, as at a point (`a_key_after_a_restart_holds_from_the_terminals_clock`).
#[test]
fn a_break_after_a_restart_holds_a_key_only_for_its_own_hour() {
    let now = t0();
    let mut st = TurnEndState::default();
    st.seed_acts(&[50 * MIN], now);
    // The turn opened at the loop's first live read, two minutes back.
    st.break_keyed(now - 40 * MIN, now - MIN, Some(now - 2 * MIN), now);
    assert!(st.go_ahead_held(now + 15 * MIN));
    assert!(!st.go_ahead_held(now + 25 * MIN));
}

/// THE HOUR OF A KEY IN A BREAK'S TURN ENDS AN HOUR AFTER THAT BREAK (2026-10-30 review 8): the
/// person types in the turn a carry opened, which ends at a break; the agent's own wakes then end
/// at later breaks with no act typed between them (a guard holds each carry). Each wake's turn
/// opened after the key, so no later break starts the hour again.
#[test]
fn breaks_after_the_keys_turn_start_its_hour_no_later() {
    let start = t0();
    let mut st = TurnEndState::default();
    st.background_carried(
        start,
        crate::supervise::policy::cruise_carry::BACKGROUND_CARRY,
    );
    let key = start + MIN;
    st.break_keyed(key, start + 10 * MIN, Some(start), start + 10 * MIN);
    assert!(st.go_ahead_held(start + 69 * MIN));
    for wake in [2, 3] {
        let woke = start + wake * 60 * MIN;
        st.break_keyed(key, woke + MIN, Some(woke), woke + MIN);
    }
    assert!(!st.go_ahead_held(start + 71 * MIN));
}

/// AN HOUR FROM A TURN'S END IS BOOKED ONCE, AND A KEY'S OWN HOUR NOT AT ALL (2026-10-30 review
/// 8): the window's clock keeps the key, so only an hour that runs from a later turn end goes in
/// the books (`go-ahead-hour@v1`), and a fresh state seeded from it holds the go-ahead as the
/// state that booked it did. The control: the same turn read with the key at its end books
/// nothing.
#[test]
fn an_hour_from_a_turns_end_is_booked_once_and_seeded_after_a_restart() {
    let ended = t0() + 90 * MIN;
    let turn = |key: Duration| {
        let mut r = screened(
            Some("Commit it, but don't push until I say so."),
            "Committed on my branch; the push waits until you say so.",
            90 * MIN,
            Some(90 * MIN),
        );
        r.key = Some(key);
        r.person = Some(key);
        r
    };
    let mut st = TurnEndState::default();
    let _ = act(&mut st, &turn(90 * MIN), ended);
    assert_eq!(st.take_go_ahead_hour(), Some(ended));
    assert_eq!(st.take_go_ahead_hour(), None, "booked once");
    let mut restarted = TurnEndState::default();
    restarted.seed_go_ahead_hour(5 * MIN, ended + 5 * MIN);
    assert!(restarted.go_ahead_held(ended + 59 * MIN));
    assert!(!restarted.go_ahead_held(ended + 61 * MIN));
    assert_eq!(
        restarted.take_go_ahead_hour(),
        None,
        "a seeded hour is booked"
    );
    let mut keyed = TurnEndState::default();
    let _ = act(&mut keyed, &turn(10 * SECS), ended);
    assert!(keyed.go_ahead_held(ended + 59 * MIN));
    assert_eq!(keyed.take_go_ahead_hour(), None);
}

/// The loop's step at a point under `cfg` (`Session::decide_to_act_now`, then
/// `Session::turn_end_execute`): the decision judged for the lines its go-ahead answers, the
/// refusal read off the exception the loop raises, then any act recorded as submitted.
fn judged_under(
    st: &mut TurnEndState,
    r: &TurnEndReading,
    now: Instant,
    cfg: &SupervisorConfig,
) -> TurnEndAction {
    st.observe(r, now);
    let a = decide_turn_end(st, r, cfg, now);
    let refused = r.autopilot != Autopilot::Off
        && exception_for(st, r, cfg, now).is_some_and(|e| e.refuses());
    let verdict = st.go_ahead_verdict(&a, r, cfg, refused, now);
    st.go_ahead_judged(verdict);
    st.acted(&a, r, now);
    a
}

/// [`judged_under`] the default policy.
fn judged(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    judged_under(st, r, now, &cfg())
}

/// THE INCIDENT'S LINE ALONE GOES NOWHERE (§15.6): the go-ahead answers the reply's one line, so
/// the menu bar shows nothing from the decision that types it, and the answer says no `Needs on
/// screen`; the line still stands as the reply named it.
#[test]
fn the_incident_line_alone_shows_nothing_and_says_no_ack() {
    let mut st = TurnEndState::default();
    let a = judged(&mut st, &idle(INCIDENT, Some(3 * MIN)), t0());
    let text = go_ahead_text(&a);
    assert!(
        text.ends_with(GO_AHEAD_DROP) && !text.contains(NEEDS_ACK),
        "{text}"
    );
    assert_eq!(st.standing_needs(), needs::needs_you(INCIDENT));
    assert!(st.shown_needs().is_empty(), "{:?}", st.shown_needs());
    assert_eq!(st.shown_key(), 0);
    assert_eq!(st.needs_answered(), 1);
}

/// A REPLY THAT LEAVES THE LINE OUT KEEPS IT OFF (§15.6): a reply naming no need never cleared
/// the line, and the go-ahead answered it, so it stays off the menu bar through Cruise's
/// continuation.
#[test]
fn a_reply_that_drops_the_line_keeps_it_off() {
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&judged(
        &mut st,
        &idle(INCIDENT, Some(3 * MIN)),
        t0()
    )));
    let a = judged(
        &mut st,
        &idle(
            "Rebuilding the branch now; the suite runs next.",
            Some(3 * MIN),
        ),
        t0() + 10 * MIN,
    );
    assert!(!is_go_ahead(&a), "{a:?}");
    assert_eq!(st.standing_needs(), needs::needs_you(INCIDENT));
    assert!(st.shown_needs().is_empty(), "{:?}", st.shown_needs());
}

/// BESIDE A PERSON'S NEEDS ONLY THEIRS SHOW (§15.6): the table's first park reply names the
/// go-ahead's line, a sign-in and a publish decision; the go-ahead answers its own line alone, the
/// other two show, and the answer says they are on screen. The control: a line that asks the
/// go-ahead after a sign-in is a person's, and shows beside the go-ahead's.
#[test]
fn beside_a_persons_needs_only_theirs_show() {
    const BESIDE: &str = "The fix isn't on main yet.\n\nNEEDS YOU: say \"rerun the checks\" when \
                          the Mac has memory to spare (the run on the merged branch was stopped \
                          for low memory and I'm not restarting it on my own).\nNEEDS YOU: sign \
                          agy and Muse in on this Mac (browser OAuth; run `aterm ai doctor` at a \
                          terminal).\nNEEDS YOU: decide on public hosting of the signed fleet \
                          document (outward-facing publish).";
    let mut st = TurnEndState::default();
    let text = go_ahead_text(&judged(&mut st, &idle(BESIDE, Some(3 * MIN)), t0())).to_string();
    assert!(
        text.contains(GO_AHEAD_OTHERS) && text.contains(NEEDS_ACK),
        "{text}"
    );
    let named = needs::needs_you(BESIDE);
    assert_eq!(st.standing_needs(), named);
    assert_eq!(st.shown_needs(), &named[1..]);
    let mixed = "NEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare.\nNEEDS YOU: \
                 say go after you sign agy in, and I'll rerun the checks.";
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&judged(
        &mut st,
        &idle(mixed, Some(3 * MIN)),
        t0()
    )));
    let named = needs::needs_you(mixed);
    assert_eq!(
        st.shown_needs(),
        &named[1..],
        "NEGATIVE CONTROL: {named:#?}"
    );
}

/// A PERSON'S NEED THE MENU BAR READS AS THE PARK'S LINE SHOWS (2026-10-30 review 9): two needs
/// the base reads as one line (each opens `… when you're back so` above its own row), the first
/// the agent's reversible work, the second a credential. The go-ahead goes for the first, and the
/// one line the menu bar names stays on it.
#[test]
fn a_persons_need_read_as_the_parks_line_shows() {
    const APART: &str = "NEEDS YOU: say \"go\" when you're back so\nI can rebuild and push my \
                         branch to main.\nNEEDS YOU: say \"go\" when you're back so\nI can use \
                         your App Store Connect key for the upload.";
    let mut st = TurnEndState::default();
    let a = judged(&mut st, &idle(APART, Some(3 * MIN)), t0());
    assert!(is_go_ahead(&a), "{a:?}");
    assert_eq!(st.shown_needs(), needs::needs_you(APART), "{a:?}");
}

/// A LATER PERSON'S NEED IS NEVER HELD BY AN EARLIER GO-AHEAD (2026-10-30 review 9, second pass):
/// the go-ahead answers a park that opens `say "go" when you're back so` above `I can rebuild …`,
/// and the agent's next reply opens a credential need with the same words above `I can use your
/// App Store Connect key …`, beside another park. The menu bar reads both needs as the one line
/// the first go-ahead answered; the second go-ahead answers only the other park, so the line
/// shows. NEGATIVE CONTROL: a next reply that restates the first park keeps its line off.
#[test]
fn a_later_persons_need_is_never_held_by_an_earlier_go_ahead() {
    const FIRST: &str = "NEEDS YOU: say \"go\" when you're back so\nI can rebuild and push my \
                         branch to main.";
    const SECOND: &str = "NEEDS YOU: say \"go\" when you're back so\nI can use your App Store \
                          Connect key for the upload.\nNEEDS YOU: say \"rerun the checks\" so I \
                          can build and test my branch and push it to main.";
    let head = needs::needs_you(FIRST);
    let mut st = TurnEndState::default();
    let a = judged(&mut st, &idle(FIRST, Some(3 * MIN)), t0());
    assert!(is_go_ahead(&a), "{a:?}");
    assert!(st.shown_needs().is_empty(), "the first park's line is off");
    let mut restated = st.clone();
    let a = judged(&mut st, &idle(SECOND, Some(3 * MIN)), t0() + 10 * MIN);
    assert!(is_go_ahead(&a), "the second park's go-ahead: {a:?}");
    assert_eq!(
        st.shown_needs(),
        head,
        "the credential's line shows: {:#?}",
        st.standing_needs()
    );
    let a = judged(&mut restated, &idle(FIRST, Some(3 * MIN)), t0() + 10 * MIN);
    assert!(is_go_ahead(&a), "{a:?}");
    assert!(
        restated.shown_needs().is_empty(),
        "NEGATIVE CONTROL: the restated park's line stays off: {:#?}",
        restated.shown_needs()
    );
}

/// THE LINE WAITS UNSHOWN THROUGH THE GO-AHEAD'S OWN BACK-OFF (§15.6): the agent's short wakes put
/// the first sight of its park behind the short-turn ladder's back-off, which the go-ahead waits
/// out; the line is off the menu bar from that decision, and back on it when the answer it waited
/// for was not written.
#[test]
fn the_line_waits_unshown_through_the_go_aheads_back_off() {
    let mut now = t0();
    let mut st = TurnEndState::default();
    for k in 0..6 {
        now += 59 * MIN;
        let _ = at(
            &mut st,
            &idle(
                &format!("Checked again ({k}); nothing moved."),
                Some(30 * SECS),
            ),
            now,
        );
    }
    now += 59 * MIN;
    let parked = idle(INCIDENT, Some(30 * SECS));
    let a = judged(&mut st, &parked, now);
    assert!(
        matches!(
            a,
            TurnEndAction::WaitUntil {
                wait: WaitKind::BackOff { .. },
                ..
            }
        ),
        "{a:?}"
    );
    assert!(st.shown_needs().is_empty(), "{:?}", st.shown_needs());
    assert!(st.go_ahead_unwritten());
    assert_eq!(st.shown_needs(), needs::needs_you(INCIDENT));
    assert!(!st.go_ahead_unwritten(), "once");
}

/// A LINE THE AGENT WENT ON PAST STAYS OFF (§15.6): the go-ahead waits out its back-off on the
/// park, and before it is typed the agent's own wake ends a turn that names no need. The line it
/// parked on never goes up, neither at the point that judges the new turn nor at a break that
/// reads it (`Session::break_fold` folds it and holds no answer back, then the carry judges a turn
/// that parks nothing). NEGATIVE CONTROL: a later reply that names the line again within the
/// person's hour shows it.
#[test]
fn a_line_the_agent_went_on_past_stays_off() {
    const WENT_ON: &str = "Pushed the branch; the docs are next.";
    let line = needs::needs_you(INCIDENT);
    let mut now = t0();
    let mut st = TurnEndState::default();
    for k in 0..6 {
        now += 59 * MIN;
        let _ = at(
            &mut st,
            &idle(
                &format!("Checked again ({k}); nothing moved."),
                Some(30 * SECS),
            ),
            now,
        );
    }
    now += 59 * MIN;
    let a = judged(&mut st, &idle(INCIDENT, Some(30 * SECS)), now);
    assert!(
        matches!(
            a,
            TurnEndAction::WaitUntil {
                wait: WaitKind::BackOff { .. },
                ..
            }
        ),
        "{a:?}"
    );
    let mut at_break = st.clone();
    let a = judged(&mut st, &idle(WENT_ON, Some(30 * SECS)), now + MIN);
    assert!(!is_go_ahead(&a), "{a:?}");
    assert_eq!(st.standing_needs(), line);
    assert!(
        st.shown_needs().is_empty(),
        "at a point: {:?}",
        st.shown_needs()
    );
    at_break.fold_break_needs(None, Some(WENT_ON), false);
    at_break.go_ahead_unwritten();
    at_break.go_ahead_judged(GoAheadVerdict::Other);
    assert!(
        at_break.shown_needs().is_empty(),
        "at a break: {:?}",
        at_break.shown_needs()
    );
    let theirs = TurnEndReading {
        person: Some(8 * SECS),
        key: Some(8 * SECS),
        ..idle(INCIDENT, Some(5 * SECS))
    };
    let a = judged(&mut st, &theirs, now + 2 * MIN);
    assert!(!is_go_ahead(&a), "{a:?}");
    assert_eq!(st.shown_needs(), line, "NEGATIVE CONTROL: {a:?}");
}

/// A LINE THE GO-AHEAD DOES NOT ANSWER SHOWS (§15.6), one control per rule that stops the
/// go-ahead: answers off (the line beside the escalation), Off, a lift off Off onto Land before
/// its instruction with answers off (nothing answered survives Off; with answers on the
/// instruction answers the park afresh, [`lands_instruction_over_a_park_keeps_its_line_off`]), a
/// person's key within the hour, an irreversible reading, and an outward plan in the turn.
#[test]
fn a_line_the_go_ahead_does_not_answer_shows() {
    let need =
        "NEEDS YOU: say \"rerun the checks\" so I can rebuild the branch and push it to main.";
    let line = needs::needs_you(need);
    let shows = |what: &str, st: &TurnEndState, a: &TurnEndAction| {
        assert!(!is_go_ahead(a), "{what}: {a:?}");
        assert_eq!(st.shown_needs(), line, "{what}: {a:?}");
    };
    let mut st = TurnEndState::default();
    let a = judged_under(&mut st, &idle(need, Some(3 * MIN)), t0(), &no_answers());
    assert!(
        matches!(&a, TurnEndAction::Escalate { reason } if reason.starts_with(GO_AHEAD_WHY)),
        "{a:?}"
    );
    shows("answers off", &st, &a);
    let mut st = TurnEndState::default();
    let a = judged(
        &mut st,
        &under(Autopilot::Off, idle(need, Some(3 * MIN))),
        t0(),
    );
    shows("Off", &st, &a);
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&judged(
        &mut st,
        &idle(need, Some(3 * MIN)),
        t0()
    )));
    st.park_off();
    let a = judged_under(
        &mut st,
        &land(idle(need, Some(3 * MIN))),
        t0() + 10 * MIN,
        &no_answers(),
    );
    shows("Land after Off, answers off", &st, &a);
    for ago in [8 * SECS, 30 * MIN] {
        let mut st = TurnEndState::default();
        let theirs = TurnEndReading {
            person: Some(ago),
            key: Some(ago),
            ..idle(need, Some(5 * SECS))
        };
        let a = judged(&mut st, &theirs, t0());
        shows(&format!("a key {ago:?} ago"), &st, &a);
    }
    let irreversible = format!(
        "No force-push and nothing published. I'm still waiting for your go-ahead before \
         rebuilding.\n{need}"
    );
    let mut st = TurnEndState::default();
    let a = judged(&mut st, &idle(&irreversible, Some(3 * MIN)), t0());
    shows("an irreversible reading", &st, &a);
    let planned = TurnEndReading {
        turn_said: Some(
            "Plan: delete the stale release tags on the remote, then push my branch to main."
                .to_string(),
        ),
        ..idle(need, Some(3 * MIN))
    };
    let mut st = TurnEndState::default();
    let a = judged(&mut st, &planned, t0());
    shows("an outward plan", &st, &a);
}

/// LAND'S INSTRUCTION OVER A PARK ANSWERS ITS LINE (2026-10-30 review 9): under Land before its
/// instruction, a reply that parks gets the instruction at once, which is the act that answers
/// the park (the go-ahead follows on a park after it). So the line stays off the menu bar from
/// that decision, the instruction says no `Needs on screen` for it, and a reply that goes on as
/// told and names no need keeps it off. NEGATIVE CONTROL: with answers off the instruction answers
/// nothing, and the line shows.
#[test]
fn lands_instruction_over_a_park_keeps_its_line_off() {
    const PARKED: &str = "Committed on my branch.\nNEEDS YOU: say \"rerun the checks\" so I can \
                          build and test my branch and push it to main.";
    let line = needs::needs_you(PARKED);
    let mut st = TurnEndState::default();
    let a = judged(&mut st, &land(idle(PARKED, Some(3 * MIN))), t0());
    assert_eq!(a.rule_id(), Some(RULE_LAND), "{a:?}");
    assert!(st.shown_needs().is_empty(), "{:?}", st.shown_needs());
    assert!(
        matches!(&a, TurnEndAction::Type { text, .. } if !text.contains(NEEDS_ACK)),
        "{a:?}"
    );
    let a = judged(
        &mut st,
        &land(idle("Rebuilt, tests pass, pushing now.", Some(3 * MIN))),
        t0() + 10 * MIN,
    );
    assert_eq!(st.standing_needs(), line, "{a:?}");
    assert!(
        st.shown_needs().is_empty(),
        "the agent went on: {a:?} {:?}",
        st.shown_needs()
    );
    let mut st = TurnEndState::default();
    let a = judged_under(
        &mut st,
        &land(idle(PARKED, Some(3 * MIN))),
        t0(),
        &no_answers(),
    );
    assert_eq!(
        st.shown_needs(),
        line,
        "NEGATIVE CONTROL: answers off: {a:?}"
    );
}

/// UNDER LAND THE HOLD NAMES NO LINE THE GO-AHEAD ANSWERED (§15.6): the menu bar's Land row would
/// otherwise say a need holds the landing while the menu bar shows none. The control: a person's
/// need beside it keeps the leg.
#[test]
fn under_land_the_hold_has_no_needs_leg_for_an_answered_line() {
    use crate::harness::acts::HoldLeg;
    let held = |said: &str| {
        let mut st = TurnEndState::default();
        let first = act(
            &mut st,
            &land(idle("Started on the parser.", Some(3 * MIN))),
            t0(),
        );
        assert_eq!(first.rule_id(), Some(RULE_LAND), "{first:?}");
        let parked = over_repo(land(idle(said, Some(3 * MIN))), uncommitted_repo());
        let a = judged(&mut st, &parked, t0() + 10 * MIN);
        assert!(is_go_ahead(&a), "{a:?}");
        st.land_held().expect("Land holds")
    };
    let alone = held(INCIDENT);
    assert!(alone.read && !alone.holds(HoldLeg::Needs), "{alone:?}");
    let beside = held(&format!(
        "{INCIDENT}\nNEEDS YOU: the App Store signing credential."
    ));
    assert!(
        beside.holds(HoldLeg::Needs),
        "NEGATIVE CONTROL: a person's need: {beside:?}"
    );
}

/// The done check within the person's explicit hold.
fn checked_held(n: usize) -> String {
    super::super::go_ahead::unless_held(&done_check_words(n), true).into_owned()
}

/// Whether act `a` types text that says the go-ahead is no need right after NEEDS YOU's rule.
fn says_no_need(a: &TurnEndAction) -> bool {
    matches!(a, TurnEndAction::Type { text, .. }
        if text.contains(NEVER_A_NEED))
}

/// A DONE CHECK TYPED WITHIN THE PERSON'S HOUR SAYS NO GO-AHEAD WORD, AND ONE PAST IT DOES: a done
/// claim with no key of the person's gets the whole check; claims read after their key, within
/// [`GO_AHEAD_HELD`] of it, get it without [`NEVER_A_NEED`], at the first angle and the second, and
/// with the reversible clause where the claim names an irreversible act; a claim read an hour past
/// their key gets the whole check again. The cut never touches NEEDS YOU's rule.
#[test]
fn a_done_check_says_the_go_ahead_is_no_need_only_outside_the_persons_hour() {
    const CLAIMS: [&str; 2] = [
        "Fixed the lexer and pushed it. All done.",
        "Fixed the parser and pushed it. All done.",
    ];
    let start = t0();
    let typed = |text: String| TurnEndAction::Type {
        text,
        rule_id: RULE_DONE_CHECK,
    };
    let mut free = TurnEndState::default();
    let (a, _) = until_acted(&mut free, &idle(CLAIMS[0], Some(3 * MIN)), start);
    assert_eq!(a, typed(done_check_words(0)), "no key: the whole check");
    assert!(says_no_need(&a), "{a:?}");
    // The person's key, at `key`, as a reading at `now` measures it.
    let keyed = |said: &str, key: Instant, now: Instant| TurnEndReading {
        person: Some(now - key),
        key: Some(now - key),
        ..idle(said, Some(3 * MIN))
    };
    let key = start - 10 * MIN;
    let mut held = TurnEndState::default();
    let (a, then) = until_acted_keyed(&mut held, &keyed(CLAIMS[0], key, start), start);
    assert!(held.go_ahead_held(then), "the person's hour holds");
    assert_eq!(a, typed(checked_held(0)), "the first angle within the hour");
    let TurnEndAction::Type { text, .. } = &a else {
        panic!("{a:?}");
    };
    assert!(!text.contains(NEVER_A_NEED), "{text}");
    let next = then + 3 * MIN;
    let (a, then) = until_acted_keyed(&mut held, &keyed(CLAIMS[1], key, next), next);
    assert!(held.go_ahead_held(then), "the person's hour still holds");
    assert_eq!(
        a,
        typed(checked_held(1)),
        "the second angle within the hour"
    );
    let old = start - GO_AHEAD_HELD - 5 * MIN;
    let mut late = TurnEndState::default();
    let (a, then) = until_acted_keyed(&mut late, &keyed(CLAIMS[0], old, start), start);
    assert!(!late.go_ahead_held(then), "past the hour");
    assert_eq!(
        a,
        typed(done_check_words(0)),
        "past the hour: the whole check"
    );
    assert!(says_no_need(&a), "{a:?}");
    let offer = "All done. Shall I delete the old release branches?";
    let mut st = TurnEndState::default();
    let (a, _) = until_acted_keyed(&mut st, &keyed(offer, key, start), start);
    assert_eq!(
        a,
        typed(format!("{} {DONE_CHECK_REVERSIBLE}", checked_held(0))),
        "the irreversible variant within the hour"
    );
}

/// A CHECK A FULL CONTEXT CUT IS TYPED AGAIN AFTER THE COMPACTION AS THE HOUR HAS IT THEN: the
/// check went whole, outside any hour; the person types while the `/compact` runs, and the
/// check typed again after it says no go-ahead word. The control: with no key, it is whole.
#[test]
fn a_check_typed_again_after_a_compaction_is_cut_within_the_persons_hour() {
    let context = |worked| walled(WallKind::Context, "Context limit reached", worked);
    let compacted = |key: Option<Duration>| TurnEndReading {
        person: key,
        key,
        ..idle("Conversation compacted.", Some(Duration::from_secs(40)))
    };
    for (key, want, says) in [
        (None, done_check_words(0), true),
        (Some(4 * MIN), checked_held(0), false),
    ] {
        let (mut st, mut now) = checked_at(t0());
        now += 5 * MIN;
        let a = act(&mut st, &context(Some(5 * MIN)), now);
        assert!(
            matches!(
                a,
                TurnEndAction::TypeCommand {
                    rule_id: RULE_COMPACT,
                    ..
                }
            ),
            "{a:?}"
        );
        now += 5 * MIN;
        let a = act(&mut st, &compacted(key), now);
        assert_eq!(
            a,
            TurnEndAction::Type {
                text: want,
                rule_id: RULE_COMPACT,
            },
            "{key:?}"
        );
        assert_eq!(says_no_need(&a), says, "{key:?}: {a:?}");
    }
}

/// THE CONTINUATION A `/compact` OWES AT A BREAK IS CUT AS THE HOUR HAS IT
/// ([`TurnEndState::owed_words`], which Cruise's carry types at a break of the agent's background
/// work in the continuation's place): the done check the full context cut, and Cruise's own
/// continuation, each without [`NEVER_A_NEED`] while the person's key holds the go-ahead, and whole
/// past their hour or with no key.
#[test]
fn a_continuation_owed_at_a_break_is_cut_within_the_persons_hour() {
    let context = |worked| walled(WallKind::Context, "Context limit reached", worked);
    let (mut st, mut now) = checked_at(t0());
    now += 5 * MIN;
    let a = act(&mut st, &context(Some(5 * MIN)), now);
    assert!(
        matches!(
            a,
            TurnEndAction::TypeCommand {
                rule_id: RULE_COMPACT,
                ..
            }
        ),
        "{a:?}"
    );
    // The agent took the `/compact`: its continuation is owed at the break that follows.
    st.took();
    assert_eq!(st.owed_at_break(), Some(RULE_COMPACT));
    assert_eq!(
        st.owed_words(RULE_COMPACT, &cfg(), None, now),
        done_check_words(0),
        "no key: the whole check"
    );
    now += MIN;
    st.observe(
        &TurnEndReading {
            person: Some(30 * SECS),
            key: Some(30 * SECS),
            ..idle("Compacting…", None)
        },
        now,
    );
    assert!(st.go_ahead_held(now), "the person's key holds the hour");
    assert_eq!(
        st.owed_words(RULE_COMPACT, &cfg(), None, now),
        checked_held(0),
        "within the hour"
    );
    assert_eq!(
        st.owed_words(RULE_COMPACT, &cfg(), None, now + GO_AHEAD_HELD),
        done_check_words(0),
        "past the hour"
    );
    let continuation = continuation_text(&cfg(), None);
    assert_eq!(
        st.owed_words(RULE_CONTINUE, &cfg(), None, now),
        super::super::go_ahead::unless_held(&continuation, true),
        "the continuation within the hour"
    );
    assert_eq!(
        st.owed_words(RULE_CONTINUE, &cfg(), None, now + GO_AHEAD_HELD),
        continuation,
        "the continuation past it"
    );
}

/// A KEY THE DECISION'S OWN `status` READ FINDS HOLDS THE HOUR AS AN OBSERVED ONE DOES
/// ([`TurnEndState::go_ahead_held_at`]): a loop that followed whose first point waited out the
/// claim's back-off reads the person's key only when it decides to type, after the point was
/// observed. The check it types then leaves [`NEVER_A_NEED`] out, as the loop that observed the
/// key does; a key an hour old, or none, leaves the check whole. NEGATIVE CONTROL: with the
/// reading's key taken out of the hold, the seed matrix's follower types the whole check where the
/// first loop typed the cut one (`seed_matrix_tests`' 529-and-full-context scenario).
#[test]
fn a_key_read_at_the_decision_holds_the_hour_as_an_observed_one() {
    let mut st = TurnEndState::default();
    st.observe(&idle("All done.", Some(3 * MIN)), t0());
    let now = t0() + st.short_wait().unwrap_or_default();
    let wake = idle("All done.", None);
    st.observe(&wake, now);
    assert!(!st.go_ahead_held(now), "no point observed a key");
    for (key, want) in [
        (None, done_check_words(0)),
        (Some(10 * MIN), checked_held(0)),
        (Some(GO_AHEAD_HELD), done_check_words(0)),
    ] {
        let read = TurnEndReading {
            key,
            ..wake.clone()
        };
        assert_eq!(
            decide_turn_end(&st, &read, &cfg(), now),
            TurnEndAction::Type {
                text: want,
                rule_id: RULE_DONE_CHECK,
            },
            "{key:?}"
        );
    }
}

/// A KEY THE DECISION'S OWN `status` READ FINDS HOLDS A PARK'S GO-AHEAD AS IT HOLDS THE
/// CONTINUATION'S SENTENCE ([`TurnEndState::go_ahead_held_at`]): a loop that followed whose first
/// point waited out a back-off reads the person's key only when it decides to type, after the
/// point was observed. A park read with a key ten minutes old gets no go-ahead there, and the hour
/// that key holds ends where the reading's key says, so the point is decided again then
/// ([`TurnEndState::go_ahead_hour_ends`]); a key an hour old, or none, gets the go-ahead.
/// NEGATIVE CONTROL: the same park with no key at all gets the go-ahead at once.
#[test]
fn a_key_read_at_the_decision_holds_a_park_as_an_observed_one() {
    const PARK: &str = "The parser is done. Nothing has changed since: the build is waiting only \
                        for your go-ahead.\n\nNEEDS YOU: say \"rerun the checks\" so I can build \
                        and test the parser branch, push it to main and remove the worktree.";
    let mut st = TurnEndState::default();
    st.observe(&idle(PARK, Some(3 * MIN)), t0());
    let now = t0() + st.short_wait().unwrap_or_default();
    let wake = idle(PARK, None);
    st.observe(&wake, now);
    assert!(!st.go_ahead_held(now), "no point observed a key");
    for (key, owed) in [
        (None, true),
        (Some(10 * MIN), false),
        (Some(GO_AHEAD_HELD), true),
    ] {
        let read = TurnEndReading {
            key,
            ..wake.clone()
        };
        let a = decide_turn_end(&st, &read, &cfg(), now);
        assert_eq!(is_go_ahead(&a), owed, "{key:?}: {a:?}");
        assert_eq!(st.go_ahead_held_at(&read, now), !owed, "{key:?}");
        assert_eq!(st.go_ahead_owed(&read, now).is_some(), owed, "{key:?}");
        let ends = key.filter(|_| !owed).map(|ago| now + (GO_AHEAD_HELD - ago));
        assert_eq!(st.go_ahead_hour_ends(&read, now), ends, "{key:?}");
        if let Some(ends) = ends {
            let later = TurnEndReading {
                key: key.map(|ago| ago + (ends - now)),
                ..wake.clone()
            };
            assert!(
                is_go_ahead(&decide_turn_end(&st, &later, &cfg(), ends)),
                "the park is owed once the key's hour ends"
            );
        }
    }
}

/// A CHECK THE PERSON'S HOUR CUT IS TYPED AGAIN WHOLE ONCE THEIR HOUR HAS ENDED: the done check
/// went within the hour, without [`NEVER_A_NEED`]; a full context cuts it, and the check the
/// `/compact` owes is typed again past the hour, at a point ([`TurnEndState::compact_owed_text`])
/// and at a break ([`TurnEndState::owed_words`]), with the sentence back, as a loop that seeded
/// the check types it. A second compaction's retype, the first typed within the hour, is whole
/// past it too. The control: the same check typed again within the hour stays cut.
#[test]
fn a_check_cut_within_the_persons_hour_is_typed_again_whole_past_it() {
    let context = |worked| walled(WallKind::Context, "Context limit reached", worked);
    let compacted = |key: Instant, now: Instant| TurnEndReading {
        person: Some(now - key),
        key: Some(now - key),
        ..idle("Conversation compacted.", Some(Duration::from_secs(40)))
    };
    let compact = |a: &TurnEndAction| {
        matches!(
            a,
            TurnEndAction::TypeCommand {
                rule_id: RULE_COMPACT,
                ..
            }
        )
    };
    let retyped = |text: String| TurnEndAction::Type {
        text,
        rule_id: RULE_COMPACT,
    };
    // The check typed fifty minutes after the person's key, within their hour.
    let start = t0();
    let key = start - 50 * MIN;
    let claim = |now: Instant| TurnEndReading {
        person: Some(now - key),
        key: Some(now - key),
        ..idle("All done.", Some(3 * MIN))
    };
    let held_check = |st: &mut TurnEndState| {
        let (a, then) = until_acted_keyed(st, &claim(start), start);
        assert_eq!(
            a,
            TurnEndAction::Type {
                text: checked_held(0),
                rule_id: RULE_DONE_CHECK,
            },
            "the check within the hour"
        );
        then
    };
    for (after, want) in [(5 * MIN, checked_held(0)), (15 * MIN, done_check_words(0))] {
        let mut st = TurnEndState::default();
        let mut now = held_check(&mut st);
        now += MIN;
        let a = act(&mut st, &context(Some(MIN)), now);
        assert!(compact(&a), "{a:?}");
        now += after - MIN;
        let at_break = st.clone();
        let a = act(&mut st, &compacted(key, now), now);
        assert_eq!(
            a,
            retyped(want.clone()),
            "the retype {after:?} after the check"
        );
        let mut at_break = at_break;
        at_break.took();
        assert_eq!(
            at_break.owed_words(RULE_COMPACT, &cfg(), None, now),
            want,
            "at a break {after:?} after the check"
        );
    }
    // Retyped within the hour, cut again by a second full context, and retyped past the hour.
    let mut st = TurnEndState::default();
    let mut now = held_check(&mut st);
    now += MIN;
    assert!(compact(&act(&mut st, &context(Some(MIN)), now)));
    now += MIN;
    let a = act(&mut st, &compacted(key, now), now);
    assert_eq!(
        a,
        retyped(checked_held(0)),
        "the first retype within the hour"
    );
    now += MIN;
    assert!(compact(&act(&mut st, &context(Some(MIN)), now)));
    now += 15 * MIN;
    let a = act(&mut st, &compacted(key, now), now);
    assert_eq!(
        a,
        retyped(done_check_words(0)),
        "the second retype past the hour"
    );
}

#[test]
fn a_spent_routine_decision_keeps_recovering_without_an_owner_alert() {
    let mut st = TurnEndState::default();
    let mut now = t0();
    let r = idle(INCIDENT, Some(3 * MIN));
    for _ in 0..GO_AHEAD_ACTS {
        let (action, at) = until_acted(&mut st, &r, now);
        assert!(is_go_ahead(&action), "{action:?}");
        now = at + 5 * MIN;
    }
    for _ in 0..3 {
        let (action, at) = until_acted(&mut st, &r, now);
        assert!(
            matches!(
                action,
                TurnEndAction::Type {
                    rule_id: RULE_CONTINUE,
                    ..
                }
            ),
            "{action:?}"
        );
        assert!(
            matches!(action, TurnEndAction::Type { ref text, .. } if text.contains("different safe approach"))
        );
        assert!(exception_for(&st, &r, &cfg(), at).is_none_or(|e| e.when != Urgency::Now));
        now = at + 5 * MIN;
    }
}

#[test]
fn a_semicolon_joined_landing_park_gets_go_ahead_under_cruise_and_land() {
    let landing = "Ready to commit and push; waiting on your sign-off.";
    let checks_only = "Ready to rerun the checks; waiting on your sign-off.";

    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&act(
        &mut st,
        &idle(landing, Some(3 * MIN)),
        t0()
    )));
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&act(
        &mut st,
        &idle(checks_only, Some(3 * MIN)),
        t0()
    )));

    let mut land_st = TurnEndState::default();
    assert_eq!(
        act(
            &mut land_st,
            &land(idle("Stage 1 landed.", Some(3 * MIN))),
            t0()
        ),
        land_instruction()
    );
    let (action, _) = until_acted(
        &mut land_st,
        &land(idle(landing, Some(3 * MIN))),
        t0() + 5 * MIN,
    );
    assert!(
        is_go_ahead(&action),
        "semicolon-joined commit/push park sets Park::lands under Land: {action:?}"
    );

    let mut non_landing_land_st = TurnEndState::default();
    assert_eq!(
        act(
            &mut non_landing_land_st,
            &land(idle("Stage 1 landed.", Some(3 * MIN))),
            t0()
        ),
        land_instruction()
    );
    let a = act(
        &mut non_landing_land_st,
        &land(idle(checks_only, Some(3 * MIN))),
        t0() + 5 * MIN,
    );
    assert!(
        !is_go_ahead(&a),
        "NEGATIVE CONTROL: checks-only semicolon park on a clean repo does not claim Park::lands: {a:?}"
    );
}
