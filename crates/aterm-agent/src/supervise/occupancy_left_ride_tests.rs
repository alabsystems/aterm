// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for a line left in the composer at the doors a point's act or a break's own line carries
//! (the slice-2 plan's 2.9b): the hung call's fact riding a turn end's act, a wake's riding a
//! point's, and the stuck line at a break, each written and its Enter not gone (`Written::Left`)
//! and settled at the session's next look the same way as the hold's and the wake's break lines
//! (`occupancy_left_write_tests`): sent, told; a turn seen running with the composer clear on a
//! host that serves no submit count, told; cleared unsent, released; the session gone, released.
//! And the edges: the owner's `prompt` withdrawn over a line it left (standing, never sent; sent
//! by the person, raised to no one), and the `/compact` check typed again after the hung call's
//! fact rode the first.

use super::*;
use crate::supervise::policy::turn_end::OWN_WORDS_STAND;

/// How the session's next look meets a line left at the run's first step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Met {
    /// The session's submit count risen from 5 to 6: the person's Enter sent it.
    Sent,
    /// The composer clear and the count where it was: the person cleared it.
    Cleared,
    /// No count served, and a live turn read with the composer clear before the next step.
    Turn,
    /// The session gone after the first step.
    Gone,
}

const MET: [Met; 4] = [Met::Sent, Met::Cleared, Met::Turn, Met::Gone];

/// A live turn read (the loop's `took`, under `cfg`) before the step's own, its composer clear.
fn ran_live(s: &mut Session<'_, Logged<'_>>, cfg: &SupervisorConfig) {
    s.ctl.m.screens = vec![break_screen()];
    s.ctl.m.served = 0;
    let screen = s.screen().expect("a live read");
    s.took(screen.seq, Some(cfg), &mut Heard::default());
}

/// A run of `steps` (minutes, step) under `policy` against `host`, the first write's Enter
/// refused (a person keyed before it: the line left), met at the second step as `met` says, each
/// step after the second's act taken before it.
fn left_run(
    tag: &str,
    policy: SupervisorConfig,
    host: Arc<DoorHost>,
    steps: &[(i64, Step)],
    met: Met,
) -> Both {
    let steps: Vec<(i64, Step)> = steps
        .iter()
        .take(if met == Met::Gone { 1 } else { steps.len() })
        .map(|&(m, step)| (m * 60, step))
        .collect();
    let live = policy.clone();
    Run {
        host,
        gone: met == Met::Gone,
        ..Run::new(tag, policy, &steps)
    }
    .run(&[BORN], &mut |s, _, i| match i {
        0 => {
            s.set_local_lookup(|_| Some(0));
            s.ctl
                .m
                .key_replies
                .push_back(ok("OK skipped reason=person seq=101\n"));
            if met != Met::Turn {
                s.ctl.m.submits_on = (0..64).map(|i| (i, 5)).collect();
            }
        }
        1 => match met {
            Met::Sent => s.ctl.m.submits_on = (0..64).map(|i| (i, 6)).collect(),
            Met::Turn => ran_live(s, &live),
            Met::Cleared | Met::Gone => {}
        },
        _ => took(s),
    })
}

/// The decisions and last words of `rule`'s rows (its intents left out), oldest first.
fn words_of(ran: &Both, rule: &str) -> Vec<(String, String)> {
    ran.ran
        .ledger
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(rule))
        .map(|v| {
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let reason = text("reason");
            let last = reason.rsplit(' ').next().unwrap_or("").to_string();
            (text("decision"), last)
        })
        .collect()
}

/// `(decision, last word)` pairs, owned.
fn pairs(want: &[(&str, &str)]) -> Vec<(String, String)> {
    want.iter()
        .map(|(d, w)| ((*d).to_string(), (*w).to_string()))
        .collect()
}

/// What a left line's rows say at its settle, by how it was met: its `waiting … left` row (the
/// count it was left at, where served), then its settle (`None`: none yet).
fn settled_rows(met: Met) -> Vec<(String, String)> {
    let (left, settle) = match met {
        Met::Sent => ("submits=5", ("typed", "settled=submitted")),
        Met::Cleared => ("submits=5", ("skipped", "why=cleared")),
        Met::Turn => ("left", ("typed", "settled=turn")),
        Met::Gone => ("submits=5", ("skipped", "why=gone")),
    };
    pairs(&[("waiting", left), settle])
}

/// THE HUNG CALL'S FACT LEFT WITH THE TURN END'S ACT IT RODE IS SETTLED AT THE NEXT LOOK: the act
/// written and its Enter refused, the fact is `waiting … left`; at the next point's read (looked
/// at before the policy decides) a risen count or a live turn seen with the composer clear tells
/// it (`settled=submitted|turn`, the host told once), and the next act carries it no more; a
/// composer cleared unsent releases it (`why=cleared`), and that same point's act carries it
/// again, told then; the session gone releases it (`why=gone`), told never.
#[test]
fn a_riders_fact_left_in_the_composer_is_settled_at_the_next_look() {
    for met in MET {
        let ran = left_run(
            &format!("left-rider-{met:?}"),
            keyed(Occupancy::Observe, false),
            Arc::new({
                let host = DoorHost::default();
                *host.facts.lock().expect("facts") = Some(vec![hung(BORN)]);
                host
            }),
            &[(0, Step::End), (1, Step::End)],
            met,
        );
        assert!(
            ran.sent[0].len() == 1 && ran.sent[0][0].ends_with(&riding(BORN)),
            "{met:?}: the act carried it: {:#?}",
            ran.sent
        );
        let mut want = settled_rows(met);
        if met == Met::Cleared {
            want.extend(pairs(&[("typed", "door=turn-end")]));
        }
        assert_eq!(words_of(&ran, NO_PROGRESS), want, "{met:?}");
        let informs: Vec<&String> = ran
            .ran
            .calls
            .iter()
            .filter(|c| c.starts_with("inform "))
            .collect();
        match met {
            Met::Gone => {
                assert_eq!(ran.told, [false], "{met:?}");
                assert!(informs.is_empty(), "{met:?}: {informs:#?}");
            }
            Met::Cleared => {
                assert_eq!(ran.told, [true]);
                assert!(ran.sent[1][0].ends_with(&riding(BORN)), "{:#?}", ran.sent);
                assert_eq!(informs, [&format!("inform {}", stuck(BORN))]);
            }
            Met::Sent | Met::Turn => {
                assert_eq!(ran.told, [true], "{met:?}");
                assert!(
                    !ran.sent[1].iter().any(|t| t.contains(" (aterm: ")),
                    "{met:?}: told once: {:#?}",
                    ran.sent
                );
                assert_eq!(informs, [&format!("inform {}", stuck(BORN))], "{met:?}");
            }
        }
    }
}

/// A WAKE'S FACT LEFT WITH THE POINT'S ACT IT RODE KEEPS THE POINT'S CLAIM UNTIL THE NEXT LOOK:
/// claimed through the point's door and never given back at the write, the wake rides no act and
/// is claimed by no door while the line stands; the next point's read tells it through the
/// point's door (`wake_told`, the host told once) where the count rose or a turn ran, and gives
/// the claim back (`wake_release`) where the composer was cleared, the wake riding again past the
/// session's pace, or the session went.
#[test]
fn a_wakes_fact_left_with_a_points_act_keeps_its_claim_until_the_look() {
    let sentence = {
        let ride = rider(1);
        ride.strip_prefix(" (aterm: ")
            .and_then(|r| r.strip_suffix(')'))
            .expect("the rider's sentence")
            .to_string()
    };
    let inform = format!("inform {}", templates::stuck_line(&sentence));
    for met in MET {
        let ran = left_run(
            &format!("left-point-{met:?}"),
            points(Occupancy::Prompt),
            wake_host(&[job_wake(1, WakeState::Ready)]),
            &[(0, Step::End), (1, Step::End), (22, Step::End)],
            met,
        );
        assert!(rode(&ran.sent[0], 1), "{met:?}: {:#?}", ran.sent);
        let mut want = settled_rows(met);
        let calls = wake_calls(&ran);
        match met {
            Met::Sent | Met::Turn => {
                assert_eq!(
                    calls,
                    [
                        "wake_claim #1 point".to_string(),
                        inform.clone(),
                        "wake_told #1 point".to_string(),
                    ],
                    "{met:?}"
                );
                assert!(
                    plain(&ran.sent[1]) && plain(&ran.sent[2]),
                    "{met:?}: told once: {:#?}",
                    ran.sent
                );
            }
            Met::Cleared => {
                want.extend(pairs(&[("typed", "door=point")]));
                assert_eq!(
                    calls,
                    [
                        "wake_claim #1 point".to_string(),
                        "wake_release #1 point".to_string(),
                        "wake_claim #1 point".to_string(),
                        inform.clone(),
                        "wake_told #1 point".to_string(),
                    ]
                );
                assert!(plain(&ran.sent[1]), "the pace holds it: {:#?}", ran.sent);
                assert!(rode(&ran.sent[2], 1), "past the pace: {:#?}", ran.sent);
            }
            Met::Gone => {
                assert_eq!(calls, ["wake_claim #1 point", "wake_release #1 point"]);
            }
        }
        assert_eq!(words_of(&ran, WAKE_FACT), want, "{met:?}");
    }
}

/// THE STUCK LINE LEFT AT A BREAK IS SETTLED AT THE NEXT LOOK, the break's one line meanwhile: the
/// line written under the owner's `stuck_break_note` and its Enter refused is `waiting … left`;
/// the next break's read tells it where the count rose or a turn ran (the host told once, nothing
/// typed again), and releases it where the composer was cleared, the line typed again past the
/// session's pace, or the session went.
#[test]
fn the_stuck_line_left_at_a_break_is_settled_at_the_next_look() {
    for met in MET {
        let ran = left_run(
            &format!("left-stuck-{met:?}"),
            keyed(Occupancy::Observe, true),
            Arc::new({
                let host = DoorHost::default();
                *host.facts.lock().expect("facts") = Some(vec![hung(BORN)]);
                host
            }),
            &[(0, Step::Break), (1, Step::Break), (22, Step::Break)],
            met,
        );
        assert_eq!(ran.sent[0], [stuck(BORN)], "{met:?}: {:#?}", ran.sent);
        let mut want = settled_rows(met);
        let informs = ran
            .ran
            .calls
            .iter()
            .filter(|c| c.starts_with("inform "))
            .count();
        match met {
            Met::Sent | Met::Turn => {
                assert!(
                    ran.sent[1..].iter().all(Vec::is_empty),
                    "{met:?}: told once: {:#?}",
                    ran.sent
                );
                assert_eq!(informs, 1, "{met:?}");
            }
            Met::Cleared => {
                want.extend(pairs(&[("typed", "door=break")]));
                assert!(ran.sent[1].is_empty(), "the pace holds it: {:#?}", ran.sent);
                assert_eq!(ran.sent[2], [stuck(BORN)], "past the pace: {:#?}", ran.sent);
                assert_eq!(informs, 1);
            }
            Met::Gone => assert_eq!(informs, 0),
        }
        assert_eq!(words_of(&ran, NO_PROGRESS), want, "{met:?}");
    }
}

/// THE OWNER'S `prompt` WITHDRAWN OVER A LINE IT LEFT (the slice-2 plan's 2.9b edge, decided under
/// the safety lens: nothing `prompt` typed is submitted under `observe`): a wake's line left at a
/// break, the next point's composer still holding it and the owner's mode now `observe`, the
/// policy's draft step reads the draft as aterm's own words (`Composer::Own`) and asks a person
/// (`OWN_WORDS_STAND`), pressing no key, the line still left. NEGATIVE CONTROLS: under `prompt`
/// still, the draft step sends it (`TurnEndAction::Submit`), and its Enter tells it
/// (`settled=submitted`, through the break's door); `SubmitWithoutPrompt` sends it under
/// `observe` too.
#[test]
fn a_line_prompt_left_is_never_sent_once_prompt_is_withdrawn() {
    let run = |tag: &str, withdrawn: bool| {
        Run {
            host: wake_host(&[job_wake(1, WakeState::Ready)]),
            standing: Some(1),
            policy_from: withdrawn.then(|| (1, points(Occupancy::Observe))),
            ..Run::new(
                tag,
                points(Occupancy::Prompt),
                &[(0, Step::Break), (6 * 60, Step::End)],
            )
        }
        .run(&[], &mut |s, _, i| {
            if i == 0 {
                s.set_local_lookup(|_| Some(0));
                s.ctl
                    .m
                    .key_replies
                    .push_back(ok("OK skipped reason=person seq=101\n"));
            }
        })
    };
    let enters = |ran: &Both| {
        ran.ran
            .requests
            .iter()
            .filter(|(r, _)| r.starts_with("key ") && r.ends_with(" enter"))
            .count()
    };
    let withdrawn = run("left-withdrawn", true);
    assert_eq!(
        withdrawn.sent[0].len(),
        1,
        "the wake's line written at the break: {:#?}",
        withdrawn.sent
    );
    assert_eq!(
        withdrawn.decided,
        [format!(
            "{:?}",
            TurnEndAction::Escalate {
                reason: OWN_WORDS_STAND.to_string()
            }
        )]
    );
    assert_eq!(
        enters(&withdrawn),
        1,
        "the break's refused Enter alone: {:#?}",
        withdrawn.ran.requests
    );
    assert_eq!(
        words_of(&withdrawn, WAKE_FACT),
        pairs(&[("waiting", "left")]),
        "still left"
    );
    let kept = run("left-withdrawn-control", false);
    assert!(
        kept.decided.len() == 1 && kept.decided[0].starts_with("Submit {"),
        "CONTROL: {:?}",
        kept.decided
    );
    assert_eq!(
        enters(&kept),
        2,
        "CONTROL: the draft step's Enter: {:#?}",
        kept.ran.requests
    );
    assert_eq!(
        words_of(&kept, WAKE_FACT),
        pairs(&[("waiting", "left"), ("typed", "settled=submitted")]),
        "CONTROL"
    );
    assert_eq!(
        wake_calls(&kept).last().map(String::as_str),
        Some("wake_told #1 break")
    );
    let planted = fault::with(Fault::SubmitWithoutPrompt, || {
        run("left-withdrawn-planted", true)
    });
    assert_eq!(
        enters(&planted),
        2,
        "NEGATIVE CONTROL: sent under observe: {:#?}",
        planted.ran.requests
    );
}

/// THE HUNG CALL'S FACT TOLD ON THE DONE CHECK NEVER RIDES THE CHECK A `/compact` TYPES AGAIN (one
/// tell per fact, as a wake's, `a_wake_told_on_the_done_check_never_rides_the_check_typed_again`):
/// the check's text the policy keeps for the compaction's continuation is kept without the fact
/// (`TurnEndState::fact_rode`), so the act typed after the cut carries no ` (aterm: `. NEGATIVE
/// CONTROL: the check typed again is the one the same run types with no hung call, and the first
/// check carried the fact.
#[test]
fn a_hung_calls_fact_told_on_the_done_check_never_rides_the_check_typed_again() {
    let run = |tag: &str, calls: Vec<BackgroundCall>| {
        let host = Arc::new(DoorHost::default());
        *host.facts.lock().expect("facts") = Some(calls);
        Run {
            host,
            ..Run::new(
                tag,
                keyed(Occupancy::Observe, false),
                &[(0, Step::Done), (60, Step::End)],
            )
        }
        .run(&[BORN], &mut |s, _, i| {
            if i > 0 {
                took(s);
                s.turn_end.seed_compact_check(RULE_DONE_CHECK);
            }
        })
    };
    let told = run("rider-compact", vec![hung(BORN)]);
    let none = run("rider-compact-none", Vec::new());
    assert!(
        told.sent[0].len() == 1 && told.sent[0][0].ends_with(&riding(BORN)),
        "the check carried it: {:#?}",
        told.sent
    );
    let check = told.sent[0][0]
        .strip_suffix(&riding(BORN))
        .expect("the check's own text");
    assert_eq!(none.sent[0], [check.to_string()], "CONTROL: the same check");
    assert_eq!(told.sent[1], [check.to_string()], "typed again without it");
    assert_eq!(none.sent[1], told.sent[1], "CONTROL");
    assert!(
        told.ran
            .ledger
            .lines()
            .any(|l| l.contains(&format!("\"rule_id\":\"{RULE_COMPACT}\""))),
        "the compaction's continuation: {}",
        told.ran.ledger
    );
    assert_eq!(told.told, [true]);
    assert_eq!(told.facts("turn-end"), ["typed"]);
}

/// A LINE `prompt` LEFT AND SENT ONCE `prompt` IS WITHDRAWN IS TOLD AND RAISED TO NO ONE (the
/// 2.9b test review's F2; the safety lens: nothing `prompt` typed is raised under `observe`): a
/// wake's line left at a break, the person's Enter sending it before the next break and the
/// owner's mode `observe` there, its settle books it told (`settled=submitted`) and tells the
/// host no line (`IdleHost::inform`). CONTROL: `prompt` kept, the same settle tells the host the
/// line once. NEGATIVE CONTROL: `InformUngranted` tells it under `observe`.
#[test]
fn a_line_prompt_left_and_sent_once_prompt_is_withdrawn_is_raised_to_no_one() {
    let run = |tag: &str, withdrawn: bool| {
        Run {
            host: wake_host(&[job_wake(1, WakeState::Ready)]),
            policy_from: withdrawn.then(|| (1, points(Occupancy::Observe))),
            ..Run::new(
                tag,
                points(Occupancy::Prompt),
                &[(0, Step::Break), (6 * 60, Step::Break)],
            )
        }
        .run(&[], &mut |s, _, i| {
            if i == 0 {
                s.set_local_lookup(|_| Some(0));
                s.ctl
                    .m
                    .key_replies
                    .push_back(ok("OK skipped reason=person seq=101\n"));
                s.ctl.m.submits_on = (0..64).map(|i| (i, 5)).collect();
            } else {
                s.ctl.m.submits_on = (0..64).map(|i| (i, 6)).collect();
            }
        })
    };
    let informs = |ran: &Both| {
        ran.ran
            .calls
            .iter()
            .filter(|c| c.starts_with("inform "))
            .count()
    };
    let withdrawn = run("left-sent-withdrawn", true);
    assert_eq!(
        words_of(&withdrawn, WAKE_FACT),
        pairs(&[("waiting", "submits=5"), ("typed", "settled=submitted")]),
        "{:#?}",
        withdrawn.ran.calls
    );
    assert_eq!(informs(&withdrawn), 0, "{:#?}", withdrawn.ran.calls);
    let kept = run("left-sent-kept", false);
    assert_eq!(
        words_of(&kept, WAKE_FACT),
        pairs(&[("waiting", "submits=5"), ("typed", "settled=submitted")]),
        "CONTROL: {:#?}",
        kept.ran.calls
    );
    assert_eq!(informs(&kept), 1, "CONTROL: {:#?}", kept.ran.calls);
    let planted = fault::with(Fault::InformUngranted, || run("left-sent-planted", true));
    assert_eq!(
        informs(&planted),
        1,
        "NEGATIVE CONTROL: raised under observe: {:#?}",
        planted.ran.calls
    );
}

/// A WAKE TOLD AT THE LOOK PACES THE NEXT AS A TYPED ONE DOES (the 2.9b test review's F5): wake
/// 1's fact left with the point's act it rode and sent by the person, its tell at the next
/// point's look books the wake (`Books::woke`), so wake 2, due there too, waits for the session's
/// pace from that tell and rides the first point past it, as after a typed one
/// (`a_due_wake_rides_the_points_act_once_and_the_next_waits_for_the_pace`). NEGATIVE CONTROL:
/// `ToldWakeUnpaced` lets wake 2 ride the very point that told wake 1.
#[test]
fn a_wake_told_at_the_look_paces_the_next_as_a_typed_one_does() {
    let run = |tag: &str| {
        left_run(
            tag,
            points(Occupancy::Prompt),
            wake_host(&[job_wake(1, WakeState::Ready), job_wake(2, WakeState::Ready)]),
            &[(0, Step::End), (1, Step::End), (22, Step::End)],
            Met::Sent,
        )
    };
    let paced = run("left-told-paced");
    assert!(rode(&paced.sent[0], 1), "{:#?}", paced.sent);
    assert!(
        plain(&paced.sent[1]),
        "the pace holds wake 2: {:#?}",
        paced.sent
    );
    assert!(rode(&paced.sent[2], 2), "past the pace: {:#?}", paced.sent);
    let unpaced = fault::with(Fault::ToldWakeUnpaced, || run("left-told-unpaced"));
    assert!(
        rode(&unpaced.sent[1], 2),
        "NEGATIVE CONTROL: {:#?}",
        unpaced.sent
    );
}

/// THE STUCK LINE LEFT IS THE BREAK'S ONE LINE, AS A TYPED ONE IS (the 2.9b test review's F5):
/// the hung call's fact first read at 31 minutes, its line written at that break under the
/// owner's `stuck_break_note` and its Enter refused, stands as the break's line: the rule's nudge
/// due at the same break is held by it (`held:host-line`), its door making no `status` read.
/// NEGATIVE CONTROL: `StuckLeftNotTheLine` runs the nudges' door at that break, its `status` read
/// made and the nudge held only by the person's keys (`held:person`).
#[test]
fn the_stuck_line_left_is_the_breaks_one_line() {
    let run = |tag: &str| {
        Run::new(
            tag,
            keyed(Occupancy::Prompt, true),
            &[(0, Step::Break), (31 * 60, Step::Break)],
        )
        .run(&[BORN], &mut |s, host, i| {
            if i == 0 {
                *host.facts.lock().expect("facts") = Some(Vec::new());
            } else {
                *host.facts.lock().expect("facts") = Some(vec![hung(BORN)]);
                s.ctl
                    .m
                    .key_replies
                    .push_back(ok("OK skipped reason=person seq=101\n"));
            }
        })
    };
    let nudge = |ran: &Both| -> Vec<String> {
        ran.ran
            .noted
            .iter()
            .filter(|n| n.starts_with("OCCUPANCY ") && n.contains(" rule=wait-nudge@v1 "))
            .map(|n| n.rsplit(' ').next().unwrap_or("").to_string())
            .collect()
    };
    let statuses = |ran: &Both| {
        ran.ran
            .requests
            .iter()
            .filter(|(r, _)| r == "status")
            .count()
    };
    let one = run("stuck-left-one");
    assert_eq!(one.sent[1], [stuck(BORN)], "{:#?}", one.sent);
    assert_eq!(
        words_of(&one, NO_PROGRESS),
        pairs(&[("waiting", "left")]),
        "{:#?}",
        one.ran.calls
    );
    assert_eq!(nudge(&one), ["held:host-line"], "{:#?}", one.ran.noted);
    let two = fault::with(Fault::StuckLeftNotTheLine, || run("stuck-left-two"));
    assert_eq!(two.sent[1], [stuck(BORN)], "{:#?}", two.sent);
    assert_eq!(
        nudge(&two),
        ["held:person"],
        "NEGATIVE CONTROL: {:#?}",
        two.ran.noted
    );
    assert!(
        statuses(&two) > statuses(&one),
        "NEGATIVE CONTROL: the nudges' door's read: {:#?}",
        two.ran.requests
    );
}
