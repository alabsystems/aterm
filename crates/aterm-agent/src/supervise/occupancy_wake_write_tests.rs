// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for a wake's write at a break under `prompt` (the slice-2 plan's 2.8): §2.1's order
//! before the host's own step, the host's claim, the seed's reconcile of a predecessor's claim,
//! and the closed template.

use super::*;

/// The run's `fact-wake@v1` rows, oldest first: decision, command and reason.
fn wake_facts(ran: &Ran) -> Vec<(String, String, String)> {
    ran.ledger
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(WAKE_FACT))
        .map(|v| {
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            (text("decision"), text("command"), text("reason"))
        })
        .collect()
}

/// The run's `fact-intent@v1` rows' reasons, oldest first.
fn intents(ran: &Ran) -> Vec<String> {
    ran.rows
        .iter()
        .filter(|(rule, _, _, _)| rule == INTENT)
        .map(|(_, _, _, reason)| reason.clone())
        .collect()
}

/// The host calls of the wake's door (its claim, release and tells) and of a line's
/// bookkeeping (`break_taken`, `inform`), in order.
fn wake_door(ran: &Ran) -> Vec<String> {
    ran.calls
        .iter()
        .filter(|c| c.starts_with("wake_") || c.starts_with("inform") || *c == "break_taken")
        .cloned()
        .collect()
}

/// The `send` writes of a run.
fn sends(ran: &Ran) -> usize {
    ran.writes()
        .iter()
        .filter(|w| w.starts_with("send "))
        .count()
}

/// The run's local clock at UTC from its first break, so a typed line's clock is the test's.
fn utc(s: &mut Session<'_, Logged<'_>>, i: usize) {
    if i == 0 {
        s.set_local_lookup(|_| Some(0));
    }
}

/// The local `(HH, MM)` at `at_ms` on a clock at UTC.
fn clock_at(at_ms: i64) -> (u8, u8) {
    let minute = at_ms.div_euclid(1000).rem_euclid(86_400) / 60;
    (
        u8::try_from(minute / 60).expect("an hour"),
        u8::try_from(minute % 60).expect("a minute"),
    )
}

/// The line wake `ord` of a job's ([`job_wake`]) types, ready since `since_ms` on a clock at
/// UTC, with `slots` where its need names them.
fn job_line(ord: u16, since_ms: i64, slots: Option<(u16, u16)>) -> String {
    templates::wake(
        ord,
        Declarer::Agent,
        &[AtomKind::Pid],
        Some(clock_at(since_ms)),
        slots,
    )
    .text()
    .to_string()
}

/// The window's book of one wake of a job's, declared at [`T0`] for an agent and labelled
/// [`PLANT`], its condition held from [`T0`] + a minute, in `state`: the book a run of the
/// wake's door is held to ([`DoorHost::real`]). Its episode is [`job_wake`]'s.
fn real_book(state: WakeState) -> WakeBook {
    use crate::supervise::occupancy::declarer::AgentId;
    use crate::supervise::occupancy::wakes::WakeReq;
    let mut book = WakeBook::default();
    let req = WakeReq::parse(&format!("label={PLANT} when=pid:4242 exited")).expect("a wake");
    let agent = AgentId {
        pid: 200,
        birth_us: 2_000,
    };
    book.declare(req, Declarer::Agent, agent, T0, 0)
        .expect("declared");
    let wake = &mut book.wakes[0];
    for atom in &mut wake.when {
        atom.held = true;
    }
    wake.ready_since_ms = Some(T0 + MIN);
    wake.state = state;
    if let WakeState::Answered { at_ms } = state {
        wake.answered_ms = Some(at_ms);
    }
    book
}

/// A host whose window holds `book`.
fn real_host(book: WakeBook) -> Arc<DoorHost> {
    let host = Arc::new(DoorHost::default());
    *host.real.lock().expect("real") = Some(book);
    host
}

/// The state of wake 1 in `host`'s window's book, and the wakes an `await wake` parked there
/// now would be answered by.
fn real_after(host: &DoorHost) -> (WakeState, Vec<u16>) {
    let real = host.real.lock().expect("real");
    let book = real.as_ref().expect("a window's book");
    (book.get(1).expect("wake 1").state, book.for_await())
}

/// A host whose scripted book holds wake 1 of a job's, ready.
fn ready_host() -> Arc<DoorHost> {
    let host = Arc::new(DoorHost::default());
    *host.wakes.lock().expect("wakes") = book(&[job_wake(1, WakeState::Ready)]);
    host
}

/// THE PROMPT HALF OF T-OBS2 AND ITS OBSERVE HALF IN ONE (the slice-2 plan's §2 and 2.8): a
/// ready wake under `observe` or `off` is no write, no `status` read and no act asked of the
/// host ([`observe_types_nothing`]), its door untouched (no claim, release or tell). NEGATIVE
/// CONTROL: under `prompt` the same run types exactly one fenced line, the harness's mark first
/// and `(rule wake@v1)` last, closed words and numbers alone: its intent row before the write, the
/// claim through the break's door after the `status` read, its `fact-wake@v1 typed door=break`
/// row after the write, and the host told through the break's door.
#[test]
fn t_obs2_a_due_wake_is_typed_once_under_prompt_alone() {
    let run = |occupancy| {
        Script {
            host: ready_host(),
            ..Script::new("wake-write-t-obs2", occupancy, &[0, 5 * 60, 6 * 60])
        }
        .run(&mut |s, i| {
            utc(s, i);
            if i == 2 {
                s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default());
            }
        })
    };
    observe_types_nothing(&run);
    for occupancy in [Occupancy::Observe, Occupancy::Off] {
        let ran = run(occupancy);
        assert_eq!(wake_door(&ran), Vec::<String>::new(), "{occupancy:?}");
        assert_eq!(wake_facts(&ran), [], "{occupancy:?}");
        assert_eq!(intents(&ran), Vec::<String>::new(), "{occupancy:?}");
    }
    let ran = run(Occupancy::Prompt);
    let line = job_line(1, T0 + MIN, None);
    assert!(
        line.starts_with(crate::harness::harness_mark!())
            && line.ends_with(&format!("(rule {WAKE})")),
        "{line}"
    );
    let writes = ran.writes();
    assert_eq!(writes.len(), 2, "one line: {writes:#?}");
    assert!(
        writes[0].starts_with("send if-human=7 if-gen=1.")
            && writes[0].ends_with(&format!(" -- {line}")),
        "{writes:#?}"
    );
    assert!(
        writes[1].starts_with("key if-human=7 if-gen=1.") && writes[1].ends_with(" enter"),
        "{writes:#?}"
    );
    assert_eq!(rule_said(&ran, WAKE), ["typed"]);
    let requests = ran.requests();
    let status = requests
        .iter()
        .position(|r| r == "status")
        .expect("a status read");
    let send = requests
        .iter()
        .position(|r| r.starts_with("send "))
        .expect("a send");
    assert!(status < send, "the status read comes before the write");
    assert_eq!(
        ran.requests[send].1,
        DoorRows {
            intents: 1,
            typed: 0
        },
        "the intent booked before the write, its outcome not yet"
    );
    assert_eq!(
        wake_door(&ran),
        [
            "wake_claim #1 break".to_string(),
            "break_taken".to_string(),
            format!("inform {line}"),
            "wake_told #1 break".to_string(),
        ]
    );
    let episode = job_wake(1, WakeState::Ready).episode();
    let intent = intents(&ran);
    assert_eq!(intent.len(), 1, "{intent:?}");
    assert!(
        intent[0].starts_with(&format!("kind=wake episode={episode:016x} ord=1 held_ms="))
            && intent[0].ends_with(" door=break"),
        "{intent:?}"
    );
    let facts = wake_facts(&ran);
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        (facts[0].0.as_str(), facts[0].1.as_str()),
        ("typed", line.as_str())
    );
    assert!(
        facts[0]
            .2
            .starts_with(&format!("rule={WAKE} episode={episode:016x} held_ms="))
            && facts[0].2.ends_with(" ord=1 door=break"),
        "{facts:?}"
    );
}

/// A managed worker's ready wake under `prompt` is noted through the task door (the slice-2
/// plan's 2.4) under the wake's own word, `wake@v1#1`, and never typed or claimed: the manager
/// holds it. NEGATIVE CONTROL: the worker unmanaged, the same run types it and notes nothing.
#[test]
fn a_managed_workers_due_wake_is_noted_through_the_task_door() {
    let run = |managed: bool| {
        Script {
            host: ready_host(),
            ..Script::new(
                "wake-write-managed",
                Occupancy::Prompt,
                &[0, 5 * 60, 6 * 60],
            )
        }
        .run(&mut |s, i| {
            utc(s, i);
            s.turn_end.set_managed(managed);
        })
    };
    let ran = run(true);
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert_eq!(task_facts(&ran), [format!("task_fact {WAKE}#1")]);
    assert_eq!(rule_said(&ran, WAKE), ["held:managed"]);
    assert!(
        !ran.calls.iter().any(|c| c.starts_with("wake_claim")),
        "{:?}",
        ran.calls
    );
    let unmanaged = run(false);
    assert_eq!(sends(&unmanaged), 1, "CONTROL: {:#?}", unmanaged.noted);
    assert_eq!(task_facts(&unmanaged), Vec::<String>::new());
}

/// §2.1 AT ONE BREAK, RANKS 2 AND 3: the upgrade's notice waiting (the host's own step,
/// `at_background`) and a wake's line due, the wake's line is typed and the notice is not asked
/// for at that break: it goes at a later one, the line taken. NEGATIVE CONTROL: `HostBeforeWake` lets the notice go
/// first, and the wake's line is held by it (`held:host-line`).
#[test]
fn a_due_wake_types_before_the_upgrades_notice() {
    let run = || {
        let host = Arc::new(DoorHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.upgrade_due.store(true, Ordering::SeqCst);
        let at_break = Arc::clone(&host);
        Script {
            host,
            ..Script::new(
                "wake-write-upgrade",
                Occupancy::Prompt,
                &[0, 5 * 60, 6 * 60, 7 * 60],
            )
        }
        .run(&mut |s, i| {
            utc(s, i);
            if i == 1 {
                *at_break.wakes.lock().expect("wakes") = book(&[job_wake(1, WakeState::Ready)]);
                *at_break.notice.lock().expect("notice") = Some("the upgrade".to_string());
            }
            if i == 2 {
                s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default());
            }
        })
    };
    let ran = run();
    assert_eq!(rule_said(&ran, WAKE), ["typed"]);
    assert_eq!(sends(&ran), 1, "{:?}", ran.writes());
    let at = |ran: &Ran, call: &str| ran.calls.iter().position(|c| c == call);
    let told = at(&ran, "wake_told #1 break").expect("told");
    let notice = at(&ran, "at_background").expect("the notice goes at a later break");
    assert!(told < notice, "{:?}", ran.calls);
    let turned = fault::with(Fault::HostBeforeWake, run);
    let notice = at(&turned, "at_background").expect("the notice");
    assert!(
        turned.calls[..notice]
            .iter()
            .all(|c| !c.starts_with("wake_claim")),
        "CONTROL: {:?}",
        turned.calls
    );
    assert_eq!(
        rule_said(&turned, WAKE).first().map(String::as_str),
        Some("held:host-line"),
        "CONTROL: {:#?}",
        turned.noted
    );
}

/// THE GUARDS HOLD A WAKE'S LINE AS THEY HOLD ANY RULE'S: each guard of the slice-1 table (the
/// person and another driver, the halt, a refused `status`, unread input and a draft, a taskless
/// session, the person's exchange, the autopilot off, a stall, the host's withhold and its own
/// turn end, the memory brake, an act untaken) holds the due wake's line at the write or at the
/// decider at the break of the hold it is due at: nothing written, no claim, a held row, and the
/// `status` read the write's alone. A live turn is no door at all. NEGATIVE CONTROL: unheld, the line is typed
/// ([`t_obs2_a_due_wake_is_typed_once_under_prompt_alone`]).
#[test]
fn each_guard_holds_a_due_wake_and_nothing_is_written() {
    let cases = [
        Case {
            tag: "person",
            words: "held:person",
            set: |m| m.human_ms = Some(1_000),
            ..CASE
        },
        Case {
            tag: "driver",
            words: "held:person",
            set: |m| m.hand = "lease:another-driver",
            ..CASE
        },
        Case {
            tag: "halted",
            words: "held:halted",
            set: |m| m.hold = "1",
            ..CASE
        },
        Case {
            tag: "status-refused",
            words: "held:person",
            set: |m| {
                m.verb_replies
                    .insert("status", VecDeque::from([err("ERR busy")]));
            },
            ..CASE
        },
        Case {
            tag: "unread",
            words: "held:draft",
            set: |m| m.input = "input=pending input_bytes=3 input_wait_ms=10 fg_rss_mb=-",
            ..CASE
        },
        Case {
            tag: "draft",
            words: "held:draft",
            at_write: false,
            set: |m| m.cursor_col = Some(20),
            drawn: drafted_break,
            ..CASE
        },
        Case {
            tag: "taskless",
            words: "held:taskless",
            host: |h| h.taskless.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            tag: "exchange",
            words: "held:exchange",
            at_write: false,
            session: |s| {
                if !s.turn_end.left_to_person() {
                    let mut reading =
                        s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                    reading.left_record = true;
                    reading.autopilot = Autopilot::Land;
                    s.turn_end.observe(&reading, s.fx.mono());
                }
            },
            ..CASE
        },
        Case {
            tag: "off",
            words: "held:autopilot",
            at_write: false,
            autopilot: Some(Autopilot::Off),
            ..CASE
        },
        Case {
            tag: "stall",
            words: "held:stall",
            at_write: false,
            session: |s| {
                s.stalled = Some(crate::supervise::run::stall::WorkerStall {
                    word: "stalled",
                    bytes: Some(3),
                    wait_ms: Some(90_000),
                    rss_mb: None,
                });
            },
            ..CASE
        },
        Case {
            tag: "disk",
            words: "held:host-withheld",
            host: |h| h.withholds.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            tag: "owns",
            words: "held:host-withheld",
            host: |h| h.owns.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            tag: "memory",
            words: "held:memory",
            at_write: false,
            session: |s| s.memory_told = Some(crate::supervise::memory::Verdict::Restart),
            ..CASE
        },
        Case {
            tag: "untaken",
            words: "held:act-untaken",
            at_write: false,
            session: |s| {
                if !s.turn_end.act_untaken() {
                    let reading =
                        s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                    s.turn_end.acted(
                        &TurnEndAction::Type {
                            text: "go on".to_string(),
                            rule_id: RULE_CONTINUE,
                        },
                        &reading,
                        s.fx.mono(),
                    );
                }
            },
            ..CASE
        },
    ];
    for case in &cases {
        let tag = format!("wake-write-guard-{}", case.tag);
        let host = Arc::new(DoorHost::default());
        (case.host)(&host);
        let at_break = Arc::clone(&host);
        let mut script = Script {
            drawn: &|_| (case.drawn)(),
            set: &case.set,
            host,
            ..Script::new(&tag, Occupancy::Prompt, &[0, 5 * 60])
        };
        if let Some(autopilot) = case.autopilot {
            script.policy.autopilot = autopilot;
        }
        let ran = script.run(&mut |s, i| {
            utc(s, i);
            if i == 1 {
                *at_break.wakes.lock().expect("wakes") = book(&[job_wake(1, WakeState::Ready)]);
            }
            (case.session)(s);
        });
        assert_eq!(
            rule_said(&ran, WAKE),
            [case.words],
            "{}: {:#?}",
            case.tag,
            ran.noted
        );
        assert!(ran.writes().is_empty(), "{}: {:?}", case.tag, ran.writes());
        assert_eq!(wake_door(&ran), Vec::<String>::new(), "{}", case.tag);
        assert_eq!(wake_facts(&ran), [], "{}", case.tag);
        let guard = case.words.strip_prefix("held:").expect("a guard");
        assert_eq!(
            held_rows_of(&ran.ledger),
            [held_by(WAKE, guard)],
            "{}",
            case.tag
        );
        assert_eq!(
            ran.requests().iter().any(|r| r == "status"),
            case.at_write,
            "{}: the status read is the write's alone",
            case.tag
        );
    }
    let live = Script {
        drawn: &|_| live_turn(),
        host: ready_host(),
        ..Script::new("wake-write-live", Occupancy::Prompt, &[0, 5 * 60])
    }
    .run(&mut utc);
    assert!(live.writes().is_empty(), "{:?}", live.writes());
    assert_eq!(wake_door(&live), Vec::<String>::new());
    assert!(
        !live.requests().iter().any(|r| r == "status"),
        "{:?}",
        live.requests()
    );
}

/// A wake's line is typed where the hold rules' are held for its rank and its modes: under Land
/// (a wake the agent declared is told under Land too, §3.2) and with the upgrade's notice due
/// (the wake outranks it, §2.1). NEGATIVE CONTROL: the autopilot off holds it
/// ([`each_guard_holds_a_due_wake_and_nothing_is_written`]'s `off`), and the same two hold a
/// nudge (`each_guard_holds_the_due_line_and_nothing_is_written`'s `land` and `upgrade`).
#[test]
fn a_land_worker_and_an_upgrade_due_type_the_wake() {
    let land = {
        let mut script = Script {
            host: ready_host(),
            ..Script::new("wake-write-land", Occupancy::Prompt, &[0, 5 * 60])
        };
        script.policy.autopilot = Autopilot::Land;
        script.run(&mut utc)
    };
    assert_eq!(rule_said(&land, WAKE), ["typed"], "{:#?}", land.noted);
    assert_eq!(sends(&land), 1);
    let host = ready_host();
    host.upgrade_due.store(true, Ordering::SeqCst);
    let upgrade = Script {
        host,
        ..Script::new("wake-write-upgrade-due", Occupancy::Prompt, &[0, 5 * 60])
    }
    .run(&mut utc);
    assert_eq!(rule_said(&upgrade, WAKE), ["typed"], "{:#?}", upgrade.noted);
    assert_eq!(sends(&upgrade), 1);
}

/// EACH ANSWER OF THE HOST'S BUT A GRANT HOLDS THE LINE AFTER ITS INTENT: a need re-read stale or
/// no longer met, the slots reserved, the machine's pace for wake lines, a refusal, another
/// door's claim, a wake gone. Each is a skip (`skipped why=<word>`, its `fact-wake@v1 skipped`
/// row after the intent), nothing written, tried again only past the session's pace; the claim is
/// given back (`wake_release`) after the answers a host may have claimed and refused, never after
/// another door's claim or a wake gone. NEGATIVE CONTROL: a grant types it
/// ([`t_obs2_a_due_wake_is_typed_once_under_prompt_alone`]).
#[test]
fn a_claim_the_host_holds_is_skipped_after_its_intent() {
    let answers = [
        (WakeClaim::Stale, true),
        (WakeClaim::Unmet, true),
        (WakeClaim::Reserved, true),
        (WakeClaim::Machine, true),
        (WakeClaim::Refused(PaceRefusal::Owns), true),
        (WakeClaim::Refused(PaceRefusal::CarryOwed), true),
        (WakeClaim::Busy, false),
        (WakeClaim::Gone, false),
    ];
    for (answer, released) in answers {
        let word = answer.word();
        let host = ready_host();
        host.claims.lock().expect("claims").push_back(answer);
        let ran = Script {
            host,
            ..Script::new(
                &format!("wake-write-claim-{}", word.replace(':', "-")),
                Occupancy::Prompt,
                &[0, 5 * 60, 6 * 60, 7 * 60],
            )
        }
        .run(&mut utc);
        assert_eq!(
            rule_said(&ran, WAKE),
            [format!("skipped why={word}")],
            "{word}: {:#?}",
            ran.noted
        );
        assert!(ran.writes().is_empty(), "{word}: {:?}", ran.writes());
        let mut door = vec!["wake_claim #1 break".to_string()];
        if released {
            door.push("wake_release #1 break".to_string());
        }
        assert_eq!(wake_door(&ran), door, "{word}: one claim in the pace");
        assert_eq!(intents(&ran).len(), 1, "{word}");
        let facts = wake_facts(&ran);
        assert_eq!(facts.len(), 1, "{word}: {facts:?}");
        assert_eq!(facts[0].0, "skipped", "{word}");
        assert!(
            facts[0].2.ends_with(&format!(" door=break why={word}")),
            "{word}: {facts:?}"
        );
    }
}

/// THE RACE: the agent's own `await wake` answers the wake between the loop's read of the book
/// and its claim, which meets `Answered`: told by the await, booked so (`told door=await`) and
/// nothing typed, released or told again by the loop. NEGATIVE CONTROL: no race, the claim is
/// granted and the line typed.
#[test]
fn a_claim_an_await_answered_first_is_told_by_it_and_nothing_typed() {
    let run = |raced: bool| {
        let host = ready_host();
        if raced {
            host.claims
                .lock()
                .expect("claims")
                .push_back(WakeClaim::Answered);
        }
        Script {
            host,
            ..Script::new("wake-write-race", Occupancy::Prompt, &[0, 5 * 60, 6 * 60])
        }
        .run(&mut utc)
    };
    let ran = run(true);
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert_eq!(rule_said(&ran, WAKE), ["told"]);
    assert_eq!(wake_door(&ran), ["wake_claim #1 break"]);
    let episode = job_wake(1, WakeState::Ready).episode();
    assert_eq!(
        wake_facts(&ran),
        [(
            "waiting".to_string(),
            String::new(),
            format!("rule={WAKE} episode={episode:016x} ord=1 told door=await")
        )]
    );
    let unraced = run(false);
    assert_eq!(sends(&unraced), 1, "CONTROL: {:#?}", unraced.noted);
}

/// AN ANSWER UNHEARD PAST THE GAP IS TOLD ONCE BY THE BREAK (the slice-2 plan's S2): the window's
/// book holds the agent's own `await wake` answer unheard past [`UNHEARD_MS`] of a hosted
/// session, and the break claims it (the one retell the book allows), types its line once, and
/// tells it through the break's door; the book then holds it told, and later breaks type nothing.
/// NEGATIVE CONTROL: heard at a live read of the harness's own turn first, it is never typed.
#[test]
fn an_answer_unheard_past_the_gap_is_typed_once_and_a_heard_one_never() {
    use crate::supervise::occupancy::wakes::UNHEARD_MS;
    let run = |live: bool| {
        let host = real_host(real_book(WakeState::Answered {
            at_ms: T0 - UNHEARD_MS + 30_000,
        }));
        let book_of = Arc::clone(&host);
        let ran = Script {
            host,
            ..Script::new(
                "wake-write-unheard",
                Occupancy::Prompt,
                &[0, 10, 20, 40, 120],
            )
        }
        .run(&mut |s, i| {
            utc(s, i);
            let cfg = policy(Occupancy::Prompt);
            if live && (i == 1 || i == 2) {
                s.turn_end.fact_typed();
                s.took(7, Some(&cfg), &mut Heard::default());
            }
            if live && i == 2 {
                let screen = screen_at(9, break_screen());
                s.hold_point(&screen, false, Some(&cfg), &mut Heard::default());
            }
        });
        (ran, real_after(&book_of))
    };
    let (unheard, (state, waiting)) = run(false);
    assert_eq!(sends(&unheard), 1, "{:#?}", unheard.noted);
    assert_eq!(
        wake_door(&unheard)
            .iter()
            .filter(|c| c.starts_with("wake_"))
            .cloned()
            .collect::<Vec<_>>(),
        ["wake_claim #1 break", "wake_told #1 break"]
    );
    assert!(
        matches!(
            state,
            WakeState::Told {
                door: WakeDoor::Break,
                ..
            }
        ),
        "{state:?}"
    );
    assert_eq!(waiting, Vec::<u16>::new());
    let (heard, (state, _)) = run(true);
    assert_eq!(sends(&heard), 0, "CONTROL: {:#?}", heard.noted);
    assert_eq!(wakes_told(&heard), ["wake_told #1 await"]);
    assert!(
        matches!(
            state,
            WakeState::Told {
                door: WakeDoor::Await,
                ..
            }
        ),
        "{state:?}"
    );
}

/// A loop's first run over the window's book of one ready wake: it types the line.
fn typed_first(tag: &str) -> Ran {
    let first = Script {
        host: real_host(real_book(WakeState::Ready)),
        ..Script::new(tag, Occupancy::Prompt, &[0, 5 * 60])
    }
    .run(&mut utc);
    assert_eq!(rule_said(&first, WAKE), ["typed"], "{:#?}", first.noted);
    first
}

/// A successor seeded with `body` at `at_ms` over a window's book whose wake the predecessor's
/// break door holds claimed (it ended before its tell reached the book): what it ran, and the
/// book after it.
fn claimed_successor(tag: &str, body: &str, at_ms: i64) -> (Ran, (WakeState, Vec<u16>)) {
    let host = real_host(real_book(WakeState::Claimed {
        door: WakeDoor::Break,
        at_ms: at_ms - 2 * MIN,
    }));
    let book_of = Arc::clone(&host);
    let ran = Script {
        host,
        seed: Some((stamped(body, at_ms - MIN), at_ms)),
        ..Script::new(tag, Occupancy::Prompt, &[0, 60])
    }
    .run(&mut utc);
    (ran, real_after(&book_of))
}

/// A CRASH AFTER THE TYPED ROW (the slice-2 plan's S4/F12): a successor finds the wake claimed by
/// its predecessor's break door, its episode's last row `typed`, and tells it through that door at
/// its first read of the book; nothing is typed again, and an `await wake` gets no answer.
/// NEGATIVE CONTROL: `ReleaseAfterTyped` gives the claim back, and the book holds the wake ready
/// for an await, a second tell.
#[test]
fn a_crash_after_the_typed_row_is_told_at_the_successors_first_read() {
    let first = typed_first("wake-write-crash-typed-1");
    let at_ms = T0 + 10 * MIN;
    let (successor, (state, waiting)) =
        claimed_successor("wake-write-crash-typed-2", &first.ledger, at_ms);
    assert_eq!(sends(&successor), 0, "{:#?}", successor.noted);
    assert_eq!(wake_door(&successor), ["wake_told #1 break"]);
    assert!(matches!(state, WakeState::Told { .. }), "{state:?}");
    assert_eq!(waiting, Vec::<u16>::new(), "an await gets no answer");
    let (released, (state, waiting)) = fault::with(Fault::ReleaseAfterTyped, || {
        claimed_successor("wake-write-crash-typed-3", &first.ledger, at_ms)
    });
    assert_eq!(wake_door(&released), ["wake_release #1 break"], "CONTROL");
    assert_eq!(state, WakeState::Ready, "CONTROL");
    assert_eq!(
        waiting,
        [1],
        "CONTROL: an await would be answered, a second tell"
    );
}

/// A CRASH AFTER THE WAKE'S INTENT ALONE: a successor finds the wake claimed, its episode's last
/// row an intent with no outcome, gives the claim back (`wake_release`, the one retell the book
/// allows) and types the line once; a loop after it types nothing. NEGATIVE CONTROL:
/// `SeedIntentAsTold` reads the intent as told, and the line is never retold.
#[test]
fn a_crash_after_the_wakes_intent_is_retold_once() {
    let first = typed_first("wake-write-crash-intent-1");
    let typed_row = |l: &&str| {
        l.contains(&format!("\"rule_id\":\"{WAKE_FACT}\"")) && l.contains("\"decision\":\"typed\"")
    };
    assert!(first.ledger.lines().any(|l| typed_row(&l)));
    let crashed: String = first
        .ledger
        .lines()
        .filter(|l| !typed_row(l))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(crashed.contains(INTENT));
    let at_ms = T0 + 10 * MIN;
    let (retold, (state, _)) = claimed_successor("wake-write-crash-intent-2", &crashed, at_ms);
    assert_eq!(sends(&retold), 1, "{:#?}", retold.noted);
    assert_eq!(
        wake_door(&retold)
            .iter()
            .filter(|c| c.starts_with("wake_"))
            .cloned()
            .collect::<Vec<_>>(),
        [
            "wake_release #1 break",
            "wake_claim #1 break",
            "wake_told #1 break"
        ]
    );
    assert!(matches!(state, WakeState::Told { .. }), "{state:?}");
    let (again, _) = claimed_successor("wake-write-crash-intent-3", &retold.ledger, at_ms + MIN);
    assert_eq!(sends(&again), 0, "told once: {:#?}", again.noted);
    let (lost, _) = fault::with(Fault::SeedIntentAsTold, || {
        claimed_successor("wake-write-crash-intent-lost", &crashed, at_ms)
    });
    assert_eq!(sends(&lost), 0, "CONTROL: {:#?}", lost.noted);
}

/// A write that writes nothing after its claim (an unfenced composer) is a skip, its
/// `fact-wake@v1 skipped` row carrying the line it would have typed, and the claim given back.
/// NEGATIVE CONTROL: fenced, the line is typed
/// ([`t_obs2_a_due_wake_is_typed_once_under_prompt_alone`]).
#[test]
fn a_wake_write_that_writes_nothing_gives_its_claim_back() {
    let ran = Script {
        set: &|m| {
            m.gen_fence = false;
            m.sends_gen = false;
            m.help = "key [id=<key>] [if=<re>] <name>: send a named key\n".to_string();
        },
        host: ready_host(),
        ..Script::new("wake-write-unfenced", Occupancy::Prompt, &[0, 5 * 60])
    }
    .run(&mut utc);
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert_eq!(rule_said(&ran, WAKE), ["skipped why=unfenced"]);
    assert_eq!(
        wake_door(&ran),
        ["wake_claim #1 break", "wake_release #1 break"]
    );
    let facts = wake_facts(&ran);
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        (facts[0].0.as_str(), facts[0].1.as_str()),
        ("skipped", job_line(1, T0 + MIN, None).as_str())
    );
    assert!(facts[0].2.ends_with(" why=unfenced"), "{facts:?}");
}

/// The line names the build slots the host's claim re-read where the wake's need names them, and
/// the claim's own ready since. NEGATIVE CONTROL: a wake whose need names no slots, the same
/// claim's slots are left out.
#[test]
fn the_wakes_line_names_the_slots_its_need_names() {
    let run = |need_slots: bool| {
        let host = Arc::new(DoorHost::default());
        *host.wakes.lock().expect("wakes") = book(&[WakeSeen {
            need: Some("slots>=1".to_string()),
            need_slots,
            ..job_wake(1, WakeState::Ready)
        }]);
        host.claims
            .lock()
            .expect("claims")
            .push_back(WakeClaim::Granted(WakeFacts {
                ready_since_ms: Some(T0 + 2 * MIN),
                slots: Some((1, 3)),
            }));
        Script {
            host,
            ..Script::new("wake-write-slots", Occupancy::Prompt, &[0, 5 * 60])
        }
        .run(&mut utc)
    };
    let typed = |ran: &Ran| {
        wake_facts(ran)
            .into_iter()
            .find(|(decision, _, _)| decision == "typed")
            .map(|(_, command, _)| command)
    };
    assert_eq!(
        typed(&run(true)),
        Some(job_line(1, T0 + 2 * MIN, Some((1, 3))))
    );
    assert_eq!(
        typed(&run(false)),
        Some(job_line(1, T0 + 2 * MIN, None)),
        "CONTROL"
    );
}

/// THE PLANT: what the agent wrote never reaches the wake's write. The window's book is labelled
/// [`PLANT`] (a label the loop's view of the book never carries), the scripted need's text is it,
/// and the screen's last words say it where a declaring command's label shows; the line is typed
/// with none of it, in the send, the told host or the typed row. NEGATIVE CONTROL:
/// `TemplateEchoesLabel` puts the screen's last words beside the template's, and the plant is
/// sent.
#[test]
fn a_planted_label_or_need_never_reaches_the_wakes_write() {
    let planted = || {
        let mut r = rows(&[
            &format!("⏺ aterm ctl @self wake label={PLANT} when=pid:4242 exited"),
            &format!("⏺ Waiting for {PLANT}."),
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    let run = |real: bool| {
        let host = if real {
            real_host(real_book(WakeState::Ready))
        } else {
            let host = Arc::new(DoorHost::default());
            *host.wakes.lock().expect("wakes") = book(&[WakeSeen {
                need: Some(PLANT.to_string()),
                ..job_wake(1, WakeState::Ready)
            }]);
            host
        };
        Script {
            drawn: &|_| planted(),
            host,
            ..Script::new("wake-write-plant", Occupancy::Prompt, &[0, 5 * 60])
        }
        .run(&mut utc)
    };
    for real in [true, false] {
        let ran = run(real);
        assert_eq!(sends(&ran), 1, "real={real}: {:#?}", ran.noted);
        assert!(
            ran.requests().iter().all(|r| !r.contains(PLANT)),
            "real={real}: {:#?}",
            ran.requests()
        );
        assert!(
            ran.calls.iter().all(|c| !c.contains(PLANT)),
            "real={real}: {:?}",
            ran.calls
        );
        assert!(
            wake_facts(&ran)
                .iter()
                .all(|(_, command, reason)| !command.contains(PLANT) && !reason.contains(PLANT)),
            "real={real}"
        );
    }
    let echoed = fault::with(Fault::TemplateEchoesLabel, || run(true));
    assert!(
        echoed
            .writes()
            .iter()
            .any(|w| w.starts_with("send ") && w.contains(PLANT)),
        "CONTROL: {:#?}",
        echoed.writes()
    );
}

/// A TYPED WAKE LINE TAKES THE SESSION'S PACE
/// ([`crate::supervise::occupancy::rules::SESSION_PACE_MS`]): the book holds two ready wakes, and
/// once the first's line is typed the second's waits out the pace however many breaks come inside
/// it. NEGATIVE CONTROL: a break past the pace types the second's line.
#[test]
fn a_typed_wake_line_paces_the_next_wakes() {
    let run = |at_s: &[i64]| {
        let host = Arc::new(DoorHost::default());
        *host.wakes.lock().expect("wakes") =
            book(&[job_wake(1, WakeState::Ready), job_wake(2, WakeState::Ready)]);
        Script {
            host,
            ..Script::new("wake-write-pace", Occupancy::Prompt, at_s)
        }
        .run(&mut |s, i| {
            utc(s, i);
            if i >= 2 {
                s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default());
            }
        })
    };
    let inside = run(&[0, 5 * 60, 6 * 60, 7 * 60, 15 * 60]);
    assert_eq!(sends(&inside), 1, "{:#?}", inside.noted);
    assert!(
        inside.writes()[0].ends_with(&format!(" -- {}", job_line(1, T0 + MIN, None))),
        "{:#?}",
        inside.writes()
    );
    let past = run(&[0, 5 * 60, 6 * 60, 7 * 60, 15 * 60, 27 * 60]);
    assert_eq!(sends(&past), 2, "CONTROL: {:#?}", past.noted);
    assert!(
        past.writes()[2].ends_with(&format!(" -- {}", job_line(2, T0 + MIN, None))),
        "CONTROL: {:#?}",
        past.writes()
    );
}

/// A TYPED WAKE LINE IS THE DOOR'S, UNTAKEN UNTIL THE AGENT TAKES IT (`TurnEndState::fact_typed`):
/// after it the turn end holds the line untaken, so the next break waits for the agent and the
/// turn that takes it is read as the harness's. NEGATIVE CONTROL: `DoorHostTyped` books it as the
/// host's notice, and no fact line stands untaken.
#[test]
fn a_typed_wake_line_stands_untaken_until_the_agent_takes_it() {
    let run = || {
        let mut untaken = Vec::new();
        let ran = Script {
            host: ready_host(),
            ..Script::new(
                "wake-write-untaken",
                Occupancy::Prompt,
                &[0, 5 * 60, 6 * 60, 7 * 60],
            )
        }
        .run(&mut |s, i| {
            utc(s, i);
            untaken.push(s.turn_end.fact_untaken());
        });
        (ran, untaken)
    };
    let (ran, untaken) = run();
    assert_eq!(sends(&ran), 1, "{:#?}", ran.noted);
    assert_eq!(untaken.first(), Some(&false), "{untaken:?}");
    assert_eq!(untaken.last(), Some(&true), "{untaken:?}");
    let (host_typed, untaken) = fault::with(Fault::DoorHostTyped, run);
    assert_eq!(sends(&host_typed), 1, "{:#?}", host_typed.noted);
    assert!(untaken.iter().all(|u| !u), "CONTROL: {untaken:?}");
}

/// A SUCCESSOR'S RECONCILE GIVES THE CLAIM BACK IN ITS OWN VIEW TOO: the one retell after a crash
/// mid-write goes at the successor's first break, the book read there holding the wake ready
/// again. NEGATIVE CONTROL: the crash's typed row kept, the same first break types nothing.
#[test]
fn a_reconciled_release_is_retold_at_the_successors_first_break() {
    let first = typed_first("wake-write-reconcile-view-1");
    let typed_row = |l: &&str| {
        l.contains(&format!("\"rule_id\":\"{WAKE_FACT}\"")) && l.contains("\"decision\":\"typed\"")
    };
    let crashed: String = first
        .ledger
        .lines()
        .filter(|l| !typed_row(l))
        .map(|l| format!("{l}\n"))
        .collect();
    let at_ms = T0 + 10 * MIN;
    let one_break = |tag: &str, body: &str| {
        let host = real_host(real_book(WakeState::Claimed {
            door: WakeDoor::Break,
            at_ms: at_ms - 2 * MIN,
        }));
        Script {
            host,
            seed: Some((stamped(body, at_ms - MIN), at_ms)),
            ..Script::new(tag, Occupancy::Prompt, &[0])
        }
        .run(&mut utc)
    };
    let retold = one_break("wake-write-reconcile-view-2", &crashed);
    assert_eq!(sends(&retold), 1, "{:#?}", retold.noted);
    let told = one_break("wake-write-reconcile-view-3", &first.ledger);
    assert_eq!(sends(&told), 0, "CONTROL: {:#?}", told.noted);
}

#[path = "occupancy_left_write_tests.rs"]
mod left;
