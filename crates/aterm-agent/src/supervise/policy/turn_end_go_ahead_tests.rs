// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE OWNER'S STANDING GO-AHEAD (go-ahead@v1; owner, 2026-10-29: "how dare you block on me for
// go ahead! fix the cruise harness so that this cannot happen again"): a park on a person's
// go-ahead for the agent's own reversible work gets it, under Cruise and Land, and nowhere else.

use super::super::go_ahead::{
    GO_AHEAD_ANSWER, GO_AHEAD_DROP, GO_AHEAD_OTHERS, GO_AHEAD_ROOM, NEVER_A_NEED,
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

#[test]
fn the_incident_reply_gets_the_owners_standing_go_ahead_with_its_word() {
    let mut st = TurnEndState::default();
    let a = act(&mut st, &idle(INCIDENT, Some(3 * MIN)), t0());
    let text = go_ahead_text(&a);
    assert_eq!(
        text,
        format!(
            "{GO_AHEAD_ANSWER} You asked the owner to say \"rerun the checks\": consider it said. \
             {GO_AHEAD_DROP} {NEEDS_ACK}"
        )
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
        text.contains("You asked the owner to say \"rerun the checks"),
        "{text}"
    );
}

#[test]
fn a_turn_a_person_prompted_keeps_todays_act() {
    let now = t0();
    let theirs = TurnEndReading {
        person: Some(8 * SECS),
        ..idle("OK, I won't push until you say so.", Some(5 * SECS))
    };
    let mut st = TurnEndState::default();
    let _ = at(&mut st, &theirs, now);
    let later = TurnEndReading {
        person: Some(3 * MIN),
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

#[test]
fn a_park_the_go_ahead_does_not_lift_is_told_now_after_three_acts() {
    let mut now = t0();
    let mut st = TurnEndState::default();
    for n in 1..=GO_AHEAD_ACTS {
        // Each refusal runs tools (a fetch, a wake scheduled): work, which no same-reply hold sees.
        let r = idle(INCIDENT, Some(3 * MIN));
        let (a, then) = until_acted(&mut st, &r, now);
        assert!(is_go_ahead(&a), "act {n}: {a:?}");
        now = then + 5 * MIN;
    }
    let r = idle(INCIDENT, Some(3 * MIN));
    assert_eq!(at(&mut st, &r, now), TurnEndAction::Nothing);
    let told = exception_for(&st, &r, &cfg(), now).expect("told");
    assert_eq!(told.when, Urgency::Now);
    assert_eq!(
        told,
        crate::supervise::policy::exception::Exception::refuses_go_ahead()
    );
    // Work between its parks lifts nothing: what clears the exception is a reply that parks no
    // more, or a person's turn.
    st.observe(&idle(INCIDENT, Some(9 * MIN)), now + 5 * MIN);
    assert_eq!(exception_for(&st, &r, &cfg(), now + 5 * MIN), Some(told));
    // A reply that parks no more starts the count again.
    let worked = idle(
        "Rebuilt, tested and pushed to main; the worktree is gone.",
        Some(9 * MIN),
    );
    let _ = act(&mut st, &worked, now + 10 * MIN);
    let (a, _) = until_acted(&mut st, &idle(INCIDENT, Some(3 * MIN)), now + 30 * MIN);
    assert!(is_go_ahead(&a), "{a:?}");
}

#[test]
fn a_park_held_at_the_same_reply_is_told_as_refusing_the_go_ahead() {
    // The incident's holds: the same reply after an act, no work between, told the person "do
    // what its last reply asks", which was the go-ahead the owner had already given.
    use crate::harness::acts::HeldBy;
    use crate::supervise::policy::exception::Exception;
    let mut now = t0();
    let mut st = TurnEndState::default();
    let mut r = idle(INCIDENT, Some(5 * SECS));
    let mut acts = 0;
    loop {
        assert!(acts <= GO_AHEAD_ACTS, "{acts} acts");
        now += 5 * SECS;
        match act(&mut st, &r, now) {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                r.worked = None;
            }
            TurnEndAction::Nothing => break,
            a => {
                assert!(is_go_ahead(&a), "{a:?}");
                acts += 1;
                r.worked = Some(5 * SECS);
            }
        }
    }
    assert_eq!(st.held_by(), Some(HeldBy::SameReply));
    r.worked = None;
    now += MIN;
    st.observe(&r, now);
    assert_eq!(
        exception_for(&st, &r, &cfg(), now),
        Some(Exception {
            held: Some(HeldBy::SameReply),
            ..Exception::refuses_go_ahead()
        })
    );
}

#[test]
fn a_park_standing_an_hour_after_its_first_go_ahead_is_told_now() {
    let now = t0();
    let mut st = TurnEndState::default();
    assert!(is_go_ahead(&act(
        &mut st,
        &idle(INCIDENT, Some(3 * MIN)),
        now
    )));
    let r = idle(INCIDENT, Some(3 * MIN));
    let at_hour = now + GO_AHEAD_SPAN;
    assert_eq!(at(&mut st, &r, at_hour), TurnEndAction::Nothing);
    assert_eq!(
        exception_for(&st, &r, &cfg(), at_hour),
        Some(crate::supervise::policy::exception::Exception::refuses_go_ahead())
    );
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

/// Every text the harness types or the docs say about the go-ahead, and the replies an agent
/// writes once it has one: none parks, stops, names a need or claims done.
#[test]
fn the_go_ahead_texts_and_their_echoes_read_as_nothing() {
    let quoted = go_ahead_answer(
        &super::super::go_ahead::parked("", INCIDENT).expect("parks"),
        true,
        None,
    );
    // The continuation and the carry say the go-ahead is no need in words a person's hold cuts.
    for said in [
        crate::supervise::config::CONTINUE_TEXT,
        crate::supervise::policy::cruise_carry::BACKGROUND_CARRY,
    ] {
        assert!(said.contains(NEVER_A_NEED), "{said}");
        let cut = super::super::go_ahead::unless_held(said, true);
        assert!(!cut.contains(NEVER_A_NEED) && !cut.contains("  "), "{cut}");
    }
    let texts = [
        GO_AHEAD_ANSWER.to_string(),
        GO_AHEAD_ROOM.to_string(),
        GO_AHEAD_DROP.to_string(),
        GO_AHEAD_OTHERS.to_string(),
        quoted,
        crate::supervise::config::CONTINUE_TEXT.to_string(),
        DONE_TAIL.to_string(),
        crate::supervise::policy::cruise_carry::BACKGROUND_CARRY.to_string(),
        "You never need a go-ahead for your own reversible work (owner ruling 2026-10-29): \
         building, testing, committing and pushing your own branch to main, or re-running a \
         check a low-memory or low-disk stop ended once there is room (`aterm ctl @self await \
         sys mem=normal`, or `'free>=room'` for disk)."
            .to_string(),
        "A go-ahead for your own reversible work is no need: Cruise and Land answer one with the \
         owner's standing go-ahead (`go-ahead@v1`)."
            .to_string(),
        "Rebuilt and pushed with your go-ahead; the suite is green and the worktree is gone."
            .to_string(),
        "I no longer need your go-ahead to rebuild, so I'm rebuilding now.".to_string(),
        "Since the go-ahead stands I'm not waiting; rebuilding the branch now.".to_string(),
        // An echo beside the go-ahead's own cleanup, its sentence negated elsewhere (2026-10-29
        // review 2).
        "Since the go-ahead stands I'm not waiting: I merged main, built and tested the branch, \
         pushed it to main and removed the worktree."
            .to_string(),
        "The harness gave the owner's standing go-ahead, so I'm rerunning the checks.".to_string(),
        "I rebuilt without waiting for your go-ahead, per the ruling.".to_string(),
        "This fix adds a go-ahead reader; its table holds rows such as:\n```\nI won't restart it \
         unless you ask.\nThe build is waiting for your go-ahead.\n```\nEach of them parks."
            .to_string(),
    ];
    for text in &texts {
        assert!(
            super::super::go_ahead::parked("", text).is_none(),
            "parks: {text}"
        );
        assert!(
            !matches!(
                classify_said(Some(text)),
                Said::Stop(_) | Said::Irreversible(_)
            ),
            "stops: {text}: {:?}",
            classify_said(Some(text))
        );
        assert!(needs::needs_you(text).is_empty(), "a need: {text}");
        assert!(!says_done(Some(text)), "done: {text}");
    }
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
    for told in [
        "Rebuilt and pushed with your go-ahead; the suite is green.",
        "Got the go-ahead, so I deleted the stale worktree I made and rebuilt.",
    ] {
        assert!(
            !matches!(
                classify_said(Some(told)),
                Said::Stop(_) | Said::Irreversible(_)
            ),
            "{told}: {:?}",
            classify_said(Some(told))
        );
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

/// A HOLD THE PERSON GAVE STANDS AFTER THEIR TURN (2026-10-29 review): Cruise's continuation goes
/// after the reply to their turn, and the replies after it that park again, restating the hold
/// or attributing it to them, get no go-ahead within the hour; an attributed hold never does.
#[test]
fn a_hold_the_person_gave_stands_in_the_replies_after_their_turn() {
    let now = t0();
    let mut st = TurnEndState::default();
    let theirs = TurnEndReading {
        person: Some(8 * SECS),
        opened_words: Some("Commit it, but don't push until I say so.".to_string()),
        ..idle("OK, I won't push until you say so.", Some(5 * SECS))
    };
    let _ = at(&mut st, &theirs, now);
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
    // Their message held the push: the hold stands past the hour, until their next turn
    // (2026-10-29 review 2).
    let restated = idle("The push still waits until you say so.", Some(3 * MIN));
    let (a, _) = until_acted(&mut st, &restated, past + 10 * MIN);
    assert!(!is_go_ahead(&a), "their hold stands: {a:?}");
    let (a, _) = until_acted(&mut st, &restated, now + 6 * GO_AHEAD_HELD);
    assert!(!is_go_ahead(&a), "their hold stands: {a:?}");
    // A person's turn that held nothing holds the go-ahead for the hour only.
    let mut st = TurnEndState::default();
    let task = TurnEndReading {
        person: Some(8 * SECS),
        opened_words: Some("Fix the parser's error recovery.".to_string()),
        ..idle(
            "Fixed the parser; the build waits for your go-ahead.",
            Some(5 * SECS),
        )
    };
    let _ = at(&mut st, &task, now);
    let (a, _) = until_acted(&mut st, &restated, past + 10 * MIN);
    assert!(is_go_ahead(&a), "the hour passed: {a:?}");
}

/// A PERSON'S TURN THE HARNESS READS FIRST (2026-10-29 review 2): after a restart a fresh state
/// knows no prompt, and the turn a person's own message opened is theirs, measured hand or not,
/// so the hold it gave stands; a turn the harness's own act opened is no person's.
#[test]
fn a_persons_turn_read_first_after_a_restart_is_theirs() {
    let said = "Understood: the branch is committed, and I'll push it once you say so.";
    let theirs = TurnEndReading {
        answered: Some(ScreenAnswer {
            prompt: 7,
            work: None,
        }),
        person: Some(10 * MIN),
        opened_words: Some(
            "Commit the branch, but hold off on the push until I say so.".to_string(),
        ),
        ..idle(said, Some(2 * MIN))
    };
    let mut st = TurnEndState::default();
    let (a, _) = until_acted(&mut st, &theirs, t0());
    assert!(!is_go_ahead(&a), "{a:?}");
    assert!(st.go_ahead_held(t0() + 2 * GO_AHEAD_HELD));
    let ours = TurnEndReading {
        opened_words: Some(format!("{GO_AHEAD_ANSWER} You parked on: \"…\".")),
        ..theirs
    };
    let mut st = TurnEndState::default();
    let (a, _) = until_acted(&mut st, &ours, t0());
    assert!(is_go_ahead(&a), "{a:?}");
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
                }),
                ..idle("Fetched main; nothing moved.", Some(3 * MIN))
            },
            t0() - 2 * GO_AHEAD_HELD,
        );
        if carried {
            st.background_carried(t0() + MIN);
        } else {
            st.fact_typed();
        }
        let r = TurnEndReading {
            answered: Some(ScreenAnswer {
                prompt: 2,
                work: None,
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
    let compliant = "Memory is still short (13 GiB of swap), so per the go-ahead I'm waiting for \
                     room first, then I'll rerun the checks.";
    let mut now = t0();
    let mut st = TurnEndState::default();
    let (a, then) = until_acted(&mut st, &idle(first, Some(3 * MIN)), now);
    assert!(go_ahead_text(&a).contains(GO_AHEAD_ROOM), "{a:?}");
    now = then + 5 * MIN;
    let start = now;
    while now < start + 2 * GO_AHEAD_SPAN {
        let r = idle(compliant, Some(3 * MIN));
        let (a, then) = until_acted(&mut st, &r, now);
        assert_ne!(a, TurnEndAction::Nothing, "at +{:?}", then - start);
        assert_ne!(
            exception_for(&st, &r, &cfg(), then),
            Some(Exception::refuses_go_ahead()),
            "at +{:?}",
            then - start
        );
        now = then + 5 * MIN;
    }
}

/// AN AGENT THAT KEEPS ITS NEEDS YOU LINE AFTER THE ANSWER REFUSES IT (2026-10-29 review 2):
/// the answer says to drop the line now, so a reply that keeps it, naming room or not, counts
/// each act, and the Now exception comes after [`GO_AHEAD_ACTS`].
#[test]
fn an_agent_that_keeps_its_room_need_is_told_now_after_three_acts() {
    let kept = idle(
        "NEEDS YOU: say \"rerun the checks\" when the Mac has memory to spare (13 GiB of swap at \
         the last read). The two builds stopped for low memory stay stopped until then, and \
         landing waits on that check.",
        Some(3 * MIN),
    );
    let mut now = t0();
    let mut st = TurnEndState::default();
    for n in 1..=GO_AHEAD_ACTS {
        let (a, then) = until_acted(&mut st, &kept, now);
        assert!(go_ahead_text(&a).contains(GO_AHEAD_ROOM), "act {n}: {a:?}");
        now = then + 5 * MIN;
    }
    assert_eq!(at(&mut st, &kept, now), TurnEndAction::Nothing);
    assert_eq!(
        exception_for(&st, &kept, &cfg(), now),
        Some(crate::supervise::policy::exception::Exception::refuses_go_ahead())
    );
}

/// A GO-AHEAD ASKED WITH NO DESTRUCTIVE ACT STILL STOPS (2026-10-29 review 2): the go-ahead's
/// stop phrases read as the base read them wherever one is asked for, marker or not, so the
/// person is told with answers off; a report, a negated need and an echo ask nothing.
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
    for told in [
        "Rebuilt and pushed with your go-ahead; the suite is green.",
        "Rebuilt and pushed to main with your go-ahead.",
        "I rebuilt without waiting for your go-ahead, per the ruling.",
        "I no longer need your go-ahead to rebuild.",
        "This adds a go-ahead reader; its rows are in the table.",
        "The rebuild that was waiting for your go-ahead is done: built, tested and pushed to main.",
    ] {
        assert!(
            !matches!(
                classify_said(Some(told)),
                Said::Stop(_) | Said::Irreversible(_)
            ),
            "{told}: {:?}",
            classify_said(Some(told))
        );
    }
}

/// A REPORT THAT NARRATES THE PARK IT CLEARED (2026-10-29 review 2): a wait in the past tense is
/// no park, so the reply gets the act it would without the go-ahead reader, never the go-ahead,
/// whose count would tell the person an agent that finished refuses it.
#[test]
fn a_report_naming_a_past_wait_gets_no_go_ahead() {
    let said = "The rebuild that was waiting for your go-ahead is done: built, tested and pushed to \
                main.";
    let done = idle(said, Some(3 * MIN));
    assert!(done.go_ahead().is_none());
    assert_eq!(classify_said(Some(said)), Said::Plain);
    let a = act(&mut TurnEndState::default(), &done, t0());
    assert_eq!(a.rule_id(), Some(RULE_CONTINUE), "{a:?}");
    let finished = idle(&format!("{said}\nATERM-TASK-DONE"), Some(3 * MIN));
    let a = act(&mut TurnEndState::default(), &finished, t0());
    assert_eq!(a.rule_id(), Some(RULE_DONE_CHECK), "{a:?}");
}

/// A PARK THAT ALTERNATES ITS NEED AND ITS PROSE IS ONE PARK (2026-10-29 review): the count runs
/// across them, so the Now exception still comes after three acts.
#[test]
fn a_park_that_alternates_its_need_and_its_prose_is_told_now_after_three_acts() {
    let prose = "Nothing has changed. The build is waiting only for your go-ahead.";
    let mut now = t0();
    let mut st = TurnEndState::default();
    for n in 0..GO_AHEAD_ACTS {
        let said = if n % 2 == 0 { INCIDENT } else { prose };
        let (a, then) = until_acted(&mut st, &idle(said, Some(3 * MIN)), now);
        assert!(is_go_ahead(&a), "act {n}: {a:?}");
        now = then + 5 * MIN;
    }
    let r = idle(prose, Some(3 * MIN));
    assert_eq!(at(&mut st, &r, now), TurnEndAction::Nothing);
    assert_eq!(
        exception_for(&st, &r, &cfg(), now),
        Some(crate::supervise::policy::exception::Exception::refuses_go_ahead())
    );
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
    let table = include_str!("fixtures/go-ahead-table.txt");
    let mut parks = false;
    let mut wrong = Vec::new();
    for line in table.lines() {
        match line {
            "== park" => parks = true,
            "== person" | "== plain" => parks = false,
            _ if !parks || line.starts_with('#') || line.trim().is_empty() => {}
            _ => {
                let r = idle(&line.replace("\\n", "\n"), Some(3 * MIN));
                let mut st = TurnEndState::default();
                let (a, _) = until_acted(&mut st, &r, t0());
                let kept = if IRREVERSIBLE_PARKS.contains(&line) {
                    a.rule_id() == Some(RULE_ANSWER)
                        && matches!(&a, TurnEndAction::Type { text, .. } if text.starts_with(REVERSIBLE_ANSWER))
                } else {
                    is_go_ahead(&a)
                };
                if !kept {
                    wrong.push(format!("{line}: {a:?}"));
                }
            }
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
