// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for a declared wake's fact riding the policy's act at an idle point (the slice-2 plan's
//! 2.9, D3): the one fact slot under `prompt`, the point's claim before the write, the policy's
//! own text on any other answer, and `observe`'s would-be row with the policy's text unchanged.

use super::*;

/// The local `(HH, MM)` at `at_ms` on a clock at UTC.
fn clock_at(at_ms: i64) -> (u8, u8) {
    let minute = at_ms.div_euclid(1000).rem_euclid(86_400) / 60;
    (
        u8::try_from(minute / 60).expect("an hour"),
        u8::try_from(minute % 60).expect("a minute"),
    )
}

/// What wake `ord` of a job's ([`job_wake`]) adds to the act it rides, on a clock at UTC.
fn rider(ord: u16) -> String {
    let line = templates::wake(
        ord,
        Declarer::Agent,
        &[AtomKind::Pid],
        Some(clock_at(T0 + MIN)),
        None,
    );
    let sentence = line.fact().strip_suffix('.').expect("a sentence");
    format!(" (aterm: {sentence})")
}

/// The ride's policy under `occupancy`: the turn-end policy continues at its points, where a
/// wake's fact rides (the rig's breaks run without Cruise's carry).
fn points(occupancy: Occupancy) -> SupervisorConfig {
    SupervisorConfig {
        continue_policy: true,
        ..policy(occupancy)
    }
}

/// A host whose scripted book holds `wakes` and whose facts reading names no hung call.
fn wake_host(wakes: &[WakeSeen]) -> Arc<DoorHost> {
    let host = Arc::new(DoorHost::default());
    *host.facts.lock().expect("facts") = Some(Vec::new());
    *host.wakes.lock().expect("wakes") = book(wakes);
    host
}

/// A run of `steps` (minutes, step) under `occupancy` against `host`, its local clock at UTC, each
/// act after the first taken before the next step but where `untaken` names the step.
fn ride_run(
    tag: &str,
    occupancy: Occupancy,
    host: Arc<DoorHost>,
    steps: &[(i64, Step)],
    untaken: &[usize],
) -> Both {
    let steps: Vec<(i64, Step)> = steps.iter().map(|&(m, step)| (m * 60, step)).collect();
    Run {
        host,
        ..Run::new(tag, points(occupancy), &steps)
    }
    .run(&[], &mut |s, _, i| {
        if i == 0 {
            s.set_local_lookup(|_| Some(0));
        }
        if i > 0 && !untaken.contains(&i) {
            took(s);
        }
    })
}

/// The run's rows of the wake's kind and its intents: rule, decision and reason, oldest first.
fn wake_rows(ran: &Both) -> Vec<(String, String, String)> {
    ran.ran
        .ledger
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .map(|v| {
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            (text("rule_id"), text("decision"), text("reason"))
        })
        .filter(|(rule, _, reason)| {
            rule == WAKE_FACT
                || (rule == INTENT && reason.starts_with("kind=wake "))
                || (rule == crate::supervise::occupancy::rules::WOULD_RULE
                    && reason.starts_with(&format!("rule={WAKE} ")))
        })
        .collect()
}

/// The host calls of the wake's door and of a line's bookkeeping, in order.
fn wake_calls(ran: &Both) -> Vec<String> {
    ran.ran
        .calls
        .iter()
        .filter(|c| c.starts_with("wake_") || c.starts_with("inform") || *c == "break_taken")
        .cloned()
        .collect()
}

fn rode(sent: &[String], ord: u16) -> bool {
    sent.len() == 1 && sent[0].ends_with(&rider(ord)) && sent[0].matches(" (aterm: ").count() == 1
}

fn plain(sent: &[String]) -> bool {
    sent.len() == 1 && !sent[0].contains(" (aterm: ")
}

/// A DUE WAKE RIDES THE POINT'S ACT ONCE UNDER `prompt` AND IS TOLD THROUGH THE POINT'S DOOR (the
/// slice-2 plan's 2.9): the first point's continuation carries the head's fact sentence once, its
/// intent row before the claim (`wake_claim(.., Point)`), its `fact-wake@v1 typed … door=point`
/// row and the host told after the write, and no break's bookkeeping (`break_taken`). The told
/// wake rides nothing again; the next wake waits for the session's pace (no claim within it, and
/// no read of the book for it) and rides the first point past it.
#[test]
fn a_due_wake_rides_the_points_act_once_and_the_next_waits_for_the_pace() {
    let host = wake_host(&[job_wake(1, WakeState::Ready), job_wake(2, WakeState::Ready)]);
    let ran = ride_run(
        "ride-once",
        Occupancy::Prompt,
        Arc::clone(&host),
        &[
            (0, Step::End),
            (1, Step::End),
            (21, Step::End),
            (22, Step::End),
        ],
        &[],
    );
    assert_eq!(
        *host.wake_reads.lock().expect("wake_reads"),
        2,
        "the book read at the points past the pace alone"
    );
    let sent = &ran.sent;
    assert!(rode(&sent[0], 1), "{sent:#?}");
    assert!(plain(&sent[1]), "the pace holds the next: {sent:#?}");
    assert!(rode(&sent[2], 2), "past the pace: {sent:#?}");
    assert!(plain(&sent[3]), "{sent:#?}");
    let told = |ord: u16| {
        let line = templates::wake(
            ord,
            Declarer::Agent,
            &[AtomKind::Pid],
            Some(clock_at(T0 + MIN)),
            None,
        );
        let sentence = line.fact().strip_suffix('.').expect("a sentence");
        format!("inform {}", templates::stuck_line(sentence))
    };
    assert_eq!(
        wake_calls(&ran),
        [
            "wake_claim #1 point".to_string(),
            told(1),
            "wake_told #1 point".to_string(),
            "wake_claim #2 point".to_string(),
            told(2),
            "wake_told #2 point".to_string(),
        ]
    );
    let rows = wake_rows(&ran);
    let words: Vec<(&str, &str)> = rows
        .iter()
        .map(|(rule, decision, _)| (rule.as_str(), decision.as_str()))
        .collect();
    assert_eq!(
        words,
        [
            (INTENT, "waiting"),
            (WAKE_FACT, "typed"),
            (INTENT, "waiting"),
            (WAKE_FACT, "typed")
        ]
    );
    assert!(
        rows.iter()
            .all(|(_, _, reason)| reason.ends_with(" door=point")),
        "{rows:#?}"
    );
    assert_eq!(ran.facts("point"), ["typed", "typed"]);
    assert!(ran.facts("break").is_empty(), "{:#?}", ran.ran.noted);
}

/// AN ANSWER BUT A GRANT TYPES THE POLICY'S OWN TEXT, SO THE ACT IS NEVER LOST TO THE RIDER: a
/// claim the agent's own `await wake` answered is booked told by it (`told door=await`), nothing
/// given back; an unmet need or the window's own refusal (its mode no longer `prompt`) is a skip,
/// the claim given back through the point's door, and the wake tried again only past the
/// session's pace. Each act is the text the same point types with no wake declared. NEGATIVE
/// CONTROL (`RideWithoutClaim`): an act that rides whatever the claim answered carries the wake
/// the agent's own `await wake` already told.
#[test]
fn an_answer_but_a_grant_types_the_policys_own_text() {
    let steps = [(0, Step::End), (1, Step::End)];
    let none = ride_run("ride-none", Occupancy::Prompt, wake_host(&[]), &steps, &[]);
    let claimed = |tag: &str, claim: WakeClaim| {
        let host = wake_host(&[job_wake(1, WakeState::Ready)]);
        host.claims.lock().expect("claims").push_back(claim);
        ride_run(tag, Occupancy::Prompt, host, &steps, &[])
    };
    let answered = claimed("ride-answered", WakeClaim::Answered);
    assert_eq!(answered.sent, none.sent, "the policy's own texts");
    assert_eq!(wake_calls(&answered), ["wake_claim #1 point"]);
    let rows = wake_rows(&answered);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str()),
        (INTENT, "waiting")
    );
    assert_eq!(
        (rows[1].0.as_str(), rows[1].1.as_str()),
        (WAKE_FACT, "waiting")
    );
    assert!(rows[1].2.ends_with(" told door=await"), "{rows:#?}");
    assert_eq!(answered.facts("point"), ["told door=await"]);
    for (tag, claim) in [
        ("ride-unmet", WakeClaim::Unmet),
        ("ride-refused", WakeClaim::Refused(PaceRefusal::Off)),
    ] {
        let skipped = claimed(tag, claim);
        assert_eq!(skipped.sent, none.sent, "{tag}: the policy's own texts");
        assert_eq!(
            wake_calls(&skipped),
            ["wake_claim #1 point", "wake_release #1 point"],
            "{tag}: given back once, and not claimed again within the pace"
        );
        let why = format!(" door=point why={}", claim.word());
        let rows = wake_rows(&skipped);
        assert!(
            rows.len() == 2 && rows[1].1 == "skipped" && rows[1].2.ends_with(&why),
            "{tag}: {rows:#?}"
        );
    }
    let again = fault::with(Fault::RideWithoutClaim, || {
        claimed("ride-without-claim", WakeClaim::Answered)
    });
    assert!(
        rode(&again.sent[0], 1),
        "NEGATIVE CONTROL: the told wake rides again: {:#?}",
        again.sent
    );
}

/// A RE-DECISION OF THE SAME POINT CARRIES NO WAKE (the slice-2 plan's D3, F11): where the point's
/// first decision is not carried out and the policy decides the point again
/// (`Session::turn_end_now`), the act it types is the policy's own and nothing is claimed.
/// NEGATIVE CONTROL: the same point's first decision carried out rides.
#[test]
fn a_re_decision_of_the_same_point_carries_no_wake() {
    let again = ride_run(
        "ride-again",
        Occupancy::Prompt,
        wake_host(&[job_wake(1, WakeState::Ready)]),
        &[(0, Step::Again)],
        &[],
    );
    assert!(plain(&again.sent[0]), "{:#?}", again.sent);
    assert!(wake_calls(&again).is_empty(), "{:?}", again.ran.calls);
    assert!(wake_rows(&again).is_empty());
    let first = ride_run(
        "ride-first",
        Occupancy::Prompt,
        wake_host(&[job_wake(1, WakeState::Ready)]),
        &[(0, Step::End)],
        &[],
    );
    assert!(rode(&first.sent[0], 1), "CONTROL: {:#?}", first.sent);
    assert_eq!(
        first.sent[0][0],
        format!("{}{}", again.sent[0][0], rider(1)),
        "the same act, the fact after it"
    );
}

/// UNDER `observe` THE POINT TYPES THE POLICY'S OWN TEXT, BYTE FOR BYTE, AND BOOKS WHAT WOULD HAVE
/// RIDDEN: a run whose book's head stands due at a point (held at the break before it by the act
/// still untaken there) types exactly what the same run types with no wake declared, claims and
/// tells nothing, reads the book at its break alone, and books one `fact-would@v1 … door=point`
/// row, once a wake. Under `off`, no row and no read. NEGATIVE CONTROL: under `prompt` the same
/// run's first point rides.
#[test]
fn under_observe_the_point_types_the_policys_own_text_and_books_its_would() {
    let steps = [
        (0, Step::End),
        (1, Step::Break),
        (2, Step::End),
        (3, Step::End),
    ];
    let run = |tag: &str, occupancy: Occupancy, wakes: &[WakeSeen]| {
        let host = wake_host(wakes);
        let ran = ride_run(tag, occupancy, Arc::clone(&host), &steps, &[1]);
        let reads = *host.wake_reads.lock().expect("wake_reads");
        (ran, reads)
    };
    let ready = [job_wake(1, WakeState::Ready)];
    let (main, _) = run("ride-observe-none", Occupancy::Observe, &[]);
    let (observed, reads) = run("ride-observe", Occupancy::Observe, &ready);
    assert_eq!(observed.sent, main.sent, "byte for byte");
    assert!(
        observed
            .sent
            .iter()
            .flatten()
            .all(|t| !t.contains(" (aterm: ")),
        "{:#?}",
        observed.sent
    );
    assert!(wake_calls(&observed).is_empty(), "{:?}", observed.ran.calls);
    assert_eq!(reads, 1, "the break's read alone");
    let rows = wake_rows(&observed);
    let would: Vec<&(String, String, String)> = rows
        .iter()
        .filter(|(rule, _, _)| rule == crate::supervise::occupancy::rules::WOULD_RULE)
        .collect();
    assert_eq!(would.len(), 1, "{rows:#?}");
    assert!(
        would[0].2.ends_with(" ord=1 door=point") && would[0].1 == "waiting",
        "{rows:#?}"
    );
    assert!(
        rows.iter().all(|(rule, _, _)| rule != INTENT),
        "nothing intended: {rows:#?}"
    );
    assert_eq!(observed.facts("point"), ["would"]);
    let (off, reads) = run("ride-off", Occupancy::Off, &ready);
    assert_eq!(off.sent, main.sent);
    assert!(wake_rows(&off).is_empty(), "{:#?}", wake_rows(&off));
    assert_eq!(reads, 0, "off reads nothing");
    let (prompted, _) = run("ride-prompt", Occupancy::Prompt, &ready);
    assert!(rode(&prompted.sent[0], 1), "CONTROL: {:#?}", prompted.sent);
}

/// A SESSION THE LOOP MEETS ONLY AT POINTS IS TOLD ITS WAKE AT THE NEXT ONE (the slice-2 plan's
/// 2.9: an agent whose reader names no background wait, as the removed Gemini CLI's did, so no
/// break ever opens a hold): a wake that turns ready between two points rides the second point's
/// act, with no break's line, hold or bookkeeping. NEGATIVE CONTROL: the first point, the wake
/// still pending, carries nothing.
#[test]
fn a_session_met_only_at_points_is_told_its_wake_at_the_next_point() {
    let host = wake_host(&[job_wake(1, WakeState::Pending)]);
    let ran = Run {
        host,
        ..Run::new(
            "ride-points-only",
            points(Occupancy::Prompt),
            &[(0, Step::End), (60, Step::End)],
        )
    }
    .run(&[], &mut |s, host, i| {
        if i == 0 {
            s.set_local_lookup(|_| Some(0));
        } else {
            took(s);
            *host.wakes.lock().expect("wakes") = book(&[job_wake(1, WakeState::Ready)]);
        }
    });
    assert!(plain(&ran.sent[0]), "CONTROL: {:#?}", ran.sent);
    assert!(rode(&ran.sent[1], 1), "{:#?}", ran.sent);
    assert!(!wake_calls(&ran).contains(&"break_taken".to_string()));
    assert!(
        ran.ran
            .noted
            .iter()
            .all(|l| !(l.starts_with("OCCUPANCY ") && l.contains(" hold open"))),
        "no hold: {:#?}",
        ran.ran.noted
    );
}

/// A point's act under the owner's mode as it stands by the write: `observe`, where the decision
/// was made under `prompt`.
fn observe_by_the_write(_: &mut Session<'_, Logged<'_>>, opts: &mut SuperviseOpts) {
    opts.policy.occupancy = Occupancy::Observe;
}

/// A point's act whose wake ([`job_wake`] 1) the session was told between the decision and the
/// write, as an answer heard there leaves it.
fn told_by_the_write(s: &mut Session<'_, Logged<'_>>, _: &mut SuperviseOpts) {
    let key = EpisodeKey {
        kind: crate::supervise::facts::FactKind::Wake,
        id: job_wake(1, WakeState::Ready).episode(),
    };
    s.facts.state.told(key, T0);
}

/// THE WRITE ASKS AGAIN WHAT THE DECISION ASKED (the slice-2 plan's W2): a point whose decision
/// carried a wake under `prompt` types the policy's own text, claims nothing and books nothing,
/// where by its write the owner's mode is `observe`, or the wake was told since the decision.
/// NEGATIVE CONTROL: with neither, the same point's act carries the wake.
#[test]
fn the_write_asks_the_owners_mode_and_the_wakes_tell_again() {
    let run = |tag: &str, wakes: &[WakeSeen], between: Option<Between<'_>>| {
        Run {
            host: wake_host(wakes),
            between,
            ..Run::new(tag, points(Occupancy::Prompt), &[(0, Step::End)])
        }
        .run(&[], &mut |s, _, _| s.set_local_lookup(|_| Some(0)))
    };
    let ready = [job_wake(1, WakeState::Ready)];
    let none = run("ride-write-none", &[], None);
    for (tag, between) in [
        ("ride-write-observe", &observe_by_the_write as Between<'_>),
        ("ride-write-told", &told_by_the_write),
    ] {
        let ran = run(tag, &ready, Some(between));
        assert!(plain(&ran.sent[0]), "{tag}: {:#?}", ran.sent);
        assert_eq!(ran.sent, none.sent, "{tag}: the policy's own text");
        assert!(wake_calls(&ran).is_empty(), "{tag}: {:?}", ran.ran.calls);
        assert!(wake_rows(&ran).is_empty(), "{tag}: {:#?}", wake_rows(&ran));
    }
    let control = run("ride-write-control", &ready, None);
    assert!(rode(&control.sent[0], 1), "CONTROL: {:#?}", control.sent);
}

/// A DOOR GUARD HOLDS THE WAKE AT A POINT WHILE THE POLICY STILL TYPES ITS ACT (the slice-2 plan's
/// 2.9: the guards and the pace before the book is read): a managed worker's continuation
/// (`Guard::Managed`) carries no wake under `prompt`, nothing is claimed and the book is not read
/// for it; under `observe` the point books no would-be row. NEGATIVE CONTROL: the same runs
/// unmanaged carry the wake under `prompt` (the book read once) and book the would-be row under
/// `observe`.
#[test]
fn a_door_guard_holds_the_wake_at_a_point_and_its_book_is_not_read() {
    let run = |tag: &str, occupancy: Occupancy, managed: bool, steps: &[(i64, Step)]| {
        let host = wake_host(&[job_wake(1, WakeState::Ready)]);
        let steps: Vec<(i64, Step)> = steps.iter().map(|&(m, step)| (m * 60, step)).collect();
        let ran = Run {
            host: Arc::clone(&host),
            ..Run::new(tag, points(occupancy), &steps)
        }
        .run(&[], &mut |s, _, i| {
            if i == 0 {
                s.set_local_lookup(|_| Some(0));
            } else if steps[i].1 != Step::Break {
                took(s);
            }
            s.turn_end.set_managed(managed);
        });
        let reads = *host.wake_reads.lock().expect("wake_reads");
        (ran, reads)
    };
    let point = [(0, Step::End)];
    let (held, reads) = run("ride-guard-managed", Occupancy::Prompt, true, &point);
    assert!(
        plain(&held.sent[0]),
        "the policy's act typed: {:#?}",
        held.sent
    );
    assert!(wake_calls(&held).is_empty(), "{:?}", held.ran.calls);
    assert!(wake_rows(&held).is_empty(), "{:#?}", wake_rows(&held));
    assert_eq!(reads, 0, "no read for a held wake");
    let (rides, reads) = run("ride-guard-control", Occupancy::Prompt, false, &point);
    assert!(rode(&rides.sent[0], 1), "CONTROL: {:#?}", rides.sent);
    assert_eq!(reads, 1, "CONTROL");
    // As the observe test runs: the break before the points held by the act still untaken there.
    let observed = [
        (0, Step::End),
        (1, Step::Break),
        (2, Step::End),
        (3, Step::End),
    ];
    let would = |ran: &Both| {
        wake_rows(ran)
            .into_iter()
            .filter(|(rule, _, reason)| {
                rule == crate::supervise::occupancy::rules::WOULD_RULE
                    && reason.ends_with(" door=point")
            })
            .count()
    };
    let (held, _) = run("ride-guard-observe", Occupancy::Observe, true, &observed);
    assert_eq!(would(&held), 0, "{:#?}", wake_rows(&held));
    assert!(held.facts("point").is_empty(), "{:#?}", held.ran.noted);
    let (control, _) = run(
        "ride-guard-observe-control",
        Occupancy::Observe,
        false,
        &observed,
    );
    assert_eq!(would(&control), 1, "CONTROL: {:#?}", wake_rows(&control));
}

/// AN ACT WHOSE WRITE TOLD NOTHING GIVES ITS WAKE BACK (the slice-2 plan's S4: only the loop
/// releases a point's claim): where the act carrying a granted wake is not written, the claim is
/// given back through the point's door, the skip booked after the intent (`fact-wake@v1 skipped
/// … door=point why=not-written`) and the wake left untold, so the first point past the
/// session's pace carries it and tells it. NEGATIVE CONTROL: the write that goes at that point
/// tells it. An act written and its Enter not gone leaves the wake to the next look
/// (`occupancy_left_ride_tests`, 2.9b).
#[test]
fn an_act_whose_write_told_nothing_gives_its_wake_back() {
    type Spoil = fn(&mut Mock);
    let cases: [(&str, &str, Spoil); 1] = [("ride-unwritten", "not-written", |m| {
        m.send_replies
            .push_back(ok("OK skipped reason=changed seq=101\n"));
    })];
    for (tag, why, set) in cases {
        let ran = Run {
            host: wake_host(&[job_wake(1, WakeState::Ready)]),
            ..Run::new(
                tag,
                points(Occupancy::Prompt),
                &[(0, Step::End), (21 * 60, Step::End)],
            )
        }
        .run(&[], &mut |s, _, i| {
            if i == 0 {
                s.set_local_lookup(|_| Some(0));
                set(s.ctl.m);
            }
        });
        assert!(
            rode(&ran.sent[0], 1),
            "{tag}: the write carried it: {:#?}",
            ran.sent
        );
        let calls = wake_calls(&ran);
        assert_eq!(
            calls.get(..2),
            Some(
                &[
                    "wake_claim #1 point".to_string(),
                    "wake_release #1 point".to_string()
                ][..]
            ),
            "{tag}: {calls:#?}"
        );
        let rows = wake_rows(&ran);
        let words: Vec<(&str, &str)> = rows
            .iter()
            .map(|(rule, decision, _)| (rule.as_str(), decision.as_str()))
            .collect();
        assert_eq!(
            words,
            [
                (INTENT, "waiting"),
                (WAKE_FACT, "skipped"),
                (INTENT, "waiting"),
                (WAKE_FACT, "typed")
            ],
            "{tag}: {rows:#?}"
        );
        assert!(
            rows[1].2.ends_with(&format!(" door=point why={why}")),
            "{tag}: {rows:#?}"
        );
        assert_eq!(
            ran.facts("point"),
            [format!("skipped why={why}"), "typed".to_string()],
            "{tag}"
        );
        assert!(rode(&ran.sent[1], 1), "CONTROL: {tag}: {:#?}", ran.sent);
        assert_eq!(
            calls.last().map(String::as_str),
            Some("wake_told #1 point"),
            "{tag}"
        );
    }
}

/// A WAKE TOLD ON THE DONE CHECK NEVER RIDES THE CHECK A `/compact` TYPES AGAIN (D3: one tell per
/// wake): the check's text the policy keeps for the compaction's continuation
/// (`TurnEndState::fact_rode`) is kept without the fact, so where a `/compact` is owed the check
/// (as a cut check leaves it, `seed_compact_check`) the act typed after it carries no
/// ` (aterm: `. NEGATIVE CONTROL: the check typed again is the one the same run types with no wake
/// declared, and the first check carried the wake.
#[test]
fn a_wake_told_on_the_done_check_never_rides_the_check_typed_again() {
    let run = |tag: &str, wakes: &[WakeSeen]| {
        Run {
            host: wake_host(wakes),
            ..Run::new(
                tag,
                points(Occupancy::Prompt),
                &[(0, Step::Done), (60, Step::End)],
            )
        }
        .run(&[], &mut |s, _, i| {
            if i == 0 {
                s.set_local_lookup(|_| Some(0));
            } else {
                took(s);
                s.turn_end.seed_compact_check(RULE_DONE_CHECK);
            }
        })
    };
    let told = run("ride-compact", &[job_wake(1, WakeState::Ready)]);
    let none = run("ride-compact-none", &[]);
    assert!(
        rode(&told.sent[0], 1),
        "the check carried it: {:#?}",
        told.sent
    );
    let check = told.sent[0][0]
        .strip_suffix(&rider(1))
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
    assert_eq!(told.facts("point"), ["typed"]);
}

/// ON A TAPE WITHOUT SLICE 2'S DOORS NO WAKE RIDES A POINT'S ACT (old tapes replay unchanged): a
/// replay whose header names a slice-1 build's doors decides a point's act with no wake under
/// `prompt` and asks the host's book nothing. NEGATIVE CONTROL: with slice 2's doors in, the same
/// act carries the book's head, its book read once.
#[test]
fn a_tape_without_slice_twos_doors_carries_no_wake_on_a_points_act() {
    use crate::supervise::run::host_door::HOST_DOORS;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::TAPE_ID;
    let opts = SuperviseOpts {
        policy: points(Occupancy::Prompt),
        ..hosted(30)
    };
    let action = TurnEndAction::Type {
        text: opts.policy.continue_text.clone(),
        rule_id: RULE_CONTINUE,
    };
    let run = |slice2: bool| {
        // No clock in the line: the replay's empty tape answers no zone read.
        let wake = WakeSeen {
            ready_since_ms: None,
            ..job_wake(1, WakeState::Ready)
        };
        let host = wake_host(&[wake]);
        let mut m = Mock::new(Vec::new());
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        let now = s.fx.mono();
        s.hold_seed_from("", now, T0, false, &opts.policy, &Heard::default());
        let doors: Vec<String> = HOST_DOORS
            .iter()
            .filter(|door| slice2 || **door != "host.wakes")
            .map(ToString::to_string)
            .collect();
        let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
        core.recorded_doors(doors);
        s.set_fx(Fx::replay(core));
        assert_eq!(s.fx.slice2_recorded(), slice2);
        let judged = screen_at(90, ended("Stage 1 of the parser is in; the lexer is next."));
        let ride = s.wake_ride(&action, &opts, now, &judged);
        let reads = *host.wake_reads.lock().expect("wake_reads");
        (ride.map(|r| r.ord()), reads)
    };
    assert_eq!(run(false), (None, 0), "nothing read, nothing carried");
    assert_eq!(run(true), (Some(1), 1), "CONTROL: slice 2's doors in");
}

#[path = "occupancy_left_ride_tests.rs"]
mod left;
