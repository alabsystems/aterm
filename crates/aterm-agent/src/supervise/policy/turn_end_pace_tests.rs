// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE PACE AROUND A WALL: a wall drawn again after a typed resume, another
// program in the tab, and the agent's own going on past its limit.

const AGY_429: &str = "agent executor error: generating and executing: Error 429, Message: You \
                       exceeded your current quota, please check your plan and billing details.";
const MUSE_LIMIT: &str = "Usage limit reached · /upgrade for increased limits, or wait for usage \
                          to reset at 1:06 PM";
const WORK_REPLY: &str = "Implemented the parser; the lexer is next.";
const NEXT_REPLY: &str = "Wrote the lexer's tests; its error paths are next.";
const DONE_CLAIM: &str = "All done: the change is committed and pushed, and every suite passes.";

/// What the policy decides at `r`, read at `now`, past the take window:
/// where it waits on the worker to take the act just typed, its decision at
/// that wait's end (the point judged there, as the loop judges it), and when.
fn judged(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> (Instant, TurnEndAction) {
    match act(st, r, now) {
        TurnEndAction::WaitUntil { until, why, .. } if why.starts_with("the worker to take") => {
            let quiet = TurnEndReading {
                worked: None,
                ..r.clone()
            };
            (until, act(st, &quiet, until))
        }
        a => (now, a),
    }
}

/// How long past `from` the wait `a` ends, or what `a` is.
fn wait_past(a: &TurnEndAction, from: Instant) -> String {
    match a {
        TurnEndAction::WaitUntil { until, why, .. } => format!(
            "waits {} s past it ({why})",
            until.saturating_duration_since(from).as_secs()
        ),
        other => format!("{other:?}"),
    }
}

/// The reset a wall drawn again names, from the instant its meet is read.
type NamedAgain<'a> = &'a dyn Fn(Instant) -> Option<Instant>;

/// The short back-off after `short` short turns in a row.
fn short_backoff(short: u32) -> Duration {
    let t = TurnEndTiming::default();
    crate::supervise::ladder::doubling(t.short_backoff, short - 1, t.short_backoff_max)
}

/// A WALL DRAWN AGAIN WITH A NEARER RESET IS WAITED OUT TO IT
/// (`resume_at_reset`): agy's per-day 429 and Muse Code's usage limit under
/// Land, each resumed past the reset it named (`usage-resume@v1`) and met
/// again by that resume with a new reset four minutes on, inside
/// `TurnEndTiming::limit_backoff`'s first rung (10 min): the loop waits to
/// that reset, the program's grace and the jitter (the margin a first wall
/// gets) and types the resume there, 101 s past it here; a floor that bound
/// it would wait the first rung from where the meet was judged, 360 s past
/// it. NEGATIVE CONTROLS: the wall drawn
/// again with the reset it named before (passed by the meet), or with none
/// (Muse Code's), waits that floor; one naming a reset past the floor waits
/// to it.
#[test]
fn a_wall_drawn_again_with_a_nearer_reset_is_waited_out_to_it() {
    let t = t0();
    let timing = TurnEndTiming::default();
    let floor = timing.limit_backoff[0];
    let r1 = t + 5 * MIN;
    let (j1, j2) = (Duration::from_secs(17), Duration::from_secs(41));
    let nearer = |meet: Instant| Some(meet + 4 * MIN);
    let named_before = |_: Instant| Some(r1);
    let unnamed = |_: Instant| None;
    let past_floor = |meet: Instant| Some(meet + 20 * MIN);
    let cases: [(&str, NamedAgain<'_>, bool); 4] = [
        ("a nearer reset", &nearer, false),
        ("control: the reset named before", &named_before, true),
        ("control: no reset named", &unnamed, true),
        ("control: a reset past the floor", &past_floor, false),
    ];
    let mut wrong = Vec::new();
    for (program, kind, said) in [
        (Program::Agy, WallKind::Spend, AGY_429),
        (Program::Muse, WallKind::UsageSession, MUSE_LIMIT),
    ] {
        let grace = timing.grace_for(program);
        let wall =
            |reset: Option<Instant>, jitter: Duration, worked: Option<Duration>| TurnEndReading {
                program,
                reset_at: reset,
                jitter,
                ..land(walled(kind, said, worked))
            };
        for (case, again, floored) in cases {
            if kind == WallKind::Spend && again(t).is_none() {
                // a spend limit naming no reset is the credits' to lift
                // (`[harness] usage_credits`), never a resume's
                continue;
            }
            let mut st = TurnEndState::default();
            let first = act(&mut st, &wall(Some(r1), j1, Some(MIN)), t);
            let typed_at = r1 + grace + j1;
            if !waits_until(&first, typed_at) {
                wrong.push(format!("{program:?} {case}: the first wall: {first:?}"));
                continue;
            }
            let resumed = act(&mut st, &wall(Some(r1), j1, None), typed_at);
            if resumed.rule_id() != Some(RULE_LIMIT_RESUME) {
                wrong.push(format!("{program:?} {case}: past the reset: {resumed:?}"));
                continue;
            }
            let meet = typed_at + Duration::from_secs(5);
            let r2 = again(meet);
            let (judged_at, a) = judged(&mut st, &wall(r2, j2, Some(Duration::from_secs(2))), meet);
            let want = match (r2, floored) {
                (_, true) => judged_at + floor,
                (Some(r2), false) => r2 + grace + j2,
                (None, false) => unreachable!("every unfloored case names a reset"),
            };
            if !waits_until(&a, want) {
                wrong.push(format!(
                    "{program:?} {case}: from the reset named: {}, want {} s",
                    wait_past(&a, r2.unwrap_or(judged_at)),
                    want.saturating_duration_since(r2.unwrap_or(judged_at))
                        .as_secs()
                ));
                continue;
            }
            let due = act(&mut st, &wall(r2, j2, None), want);
            if due.rule_id() != Some(RULE_LIMIT_RESUME) {
                wrong.push(format!("{program:?} {case}: at its due: {due:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// ANOTHER PROGRAM IN THE TAB STARTS ITS OWN SHORT STREAK
/// (`TurnEndState::observe`): agy's short turns under Land (its work point,
/// then Land's act answered short, two; or met by its per-day 429, W5's
/// shape, which adds nothing, v6 §14.17.5.7: one), and Muse Code started in
/// the tab after it: Muse Code's first short turn is its streak's first, so
/// the act on it waits one short turn's back-off, not the three short turns
/// (8 min) of a streak run on into it.
/// NEGATIVE CONTROL: agy's own next short turn (the same program, launched
/// again from the shell between, a screen no reader names) carries the streak
/// on, to three (two past the 429).
#[test]
fn another_program_in_the_tab_starts_its_own_short_streak() {
    let t = t0();
    let short = Duration::from_secs(4);
    let mut wrong = Vec::new();
    for walled_first in [false, true] {
        for next in [Program::Agy].into_iter().chain(
            Program::ALL
                .into_iter()
                .filter(|p| !matches!(p, Program::Generic | Program::Agy)),
        ) {
            let before = if walled_first { 1 } else { 2 };
            let want = if next == Program::Agy { before + 1 } else { 1 };
            let tag = format!("walled {walled_first}, then {next:?}");
            let of = |program: Program, r: TurnEndReading| TurnEndReading { program, ..land(r) };
            let mut st = TurnEndState::default();
            // Land's instruction waits no back-off: typed at the work point
            let landing = act(&mut st, &of(Program::Agy, idle(WORK_REPLY, Some(short))), t);
            if landing.rule_id() != Some(RULE_LAND) {
                wrong.push(format!("{tag}: Land's act at the work point: {landing:?}"));
                continue;
            }
            let answer = if walled_first {
                TurnEndReading {
                    reset_at: Some(t + 60 * MIN),
                    ..of(
                        Program::Agy,
                        walled(WallKind::Spend, AGY_429, Some(Duration::from_secs(2))),
                    )
                }
            } else {
                of(Program::Agy, idle(WORK_REPLY, Some(short)))
            };
            let (answered, _) = judged(&mut st, &answer, t + Duration::from_secs(5));
            if st.short_streak() != before {
                wrong.push(format!(
                    "{tag}: PRECONDITION, agy's streak {} (want {before})",
                    st.short_streak()
                ));
                continue;
            }
            // the shell the next program is launched from, a screen no
            // reader names
            at(
                &mut st,
                &of(Program::Generic, idle("", None)),
                answered + MIN,
            );
            let at_next = answered + 2 * MIN;
            // its reply another than agy's last: the same reply again is a repeat, left alone
            let a = at(&mut st, &of(next, idle(NEXT_REPLY, Some(short))), at_next);
            let backs_off = matches!(&a, TurnEndAction::WaitUntil {
                until,
                wait: WaitKind::BackOff { short },
                ..
            } if *until == at_next + short_backoff(want) && u32::from(*short) == want);
            if st.short_streak() != want || !backs_off {
                wrong.push(format!(
                    "{tag}: streak {} (want {want}), {}",
                    st.short_streak(),
                    wait_past(&a, at_next)
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// THE AGENT'S OWN GOING ON PAST ITS LIMIT IS NO SHORT TURN AGAIN
/// (`TurnEndState::observe`): under Cruise, Claude Code's done claim (one
/// short turn), Cruise's done check met by its session limit with its own
/// auto-continue armed (still one: the wall answered the act it cut, v6
/// §14.17.5.7), and Claude Code going on by itself past the reset (`Usage
/// limit reset · continuing automatically`), its turn short again: that going
/// on delivers the done check the wall cut, paced by the limit's reset as the
/// loop's own `usage-resume@v1` is, and the streak stays at one: Cruise's act
/// waits one short turn's back-off (2 min), as after the loop's resume on agy
/// and Muse Code, not the two short turns (4 min) of a count of it.
/// NEGATIVE CONTROLS: the loop's own resume typed past the reset (the
/// countdown not armed) leaves the streak at one before and after; the
/// notice drawn again at the reset, read as a turn of its own, takes nothing
/// from the act the wall cut (where it did, the continuation was a second
/// short turn); where the wall met Claude Code's own turn after Cruise's done
/// check was answered (no act of the policy's cut: two short turns), its
/// going on past the reset is a short turn (three); a later short turn of the
/// agent's own, the continuation's row still drawn, is a short turn (one
/// more).
#[test]
fn the_agents_own_going_on_past_its_limit_is_no_short_turn_again() {
    let t = t0();
    let timing = TurnEndTiming::default();
    let reset = t + 10 * MIN;
    let short = Duration::from_secs(4);
    let armed = under(
        Autopilot::Cruise,
        captured(AUTO_CONTINUE_ARMED, Some(reset), Duration::ZERO),
    );
    assert!(armed.resumes_by_itself, "PRECONDITION");
    let fired_reading = |reply: &str| TurnEndReading {
        worked: Some(Duration::from_secs(3)),
        ..under(
            Autopilot::Cruise,
            captured_rows(&replied_after_the_reset(reply, true)),
        )
    };
    assert_eq!(
        fired_reading(DONE_CLAIM).auto_continue,
        Some(aterm_phase::AutoContinue::Fired),
        "PRECONDITION"
    );
    let mut wrong = Vec::new();
    for (case, by_itself, cut, want) in [
        ("control: the loop's resume", false, true, 1),
        ("control: its own continuation, no act cut", true, false, 3),
        (
            "control: real work in the counted act's continuation",
            true,
            true,
            0,
        ),
        (
            "its own continuation of the act the wall cut",
            true,
            true,
            1,
        ),
    ] {
        let mut st = TurnEndState::default();
        let first = act(&mut st, &idle(DONE_CLAIM, Some(short)), t);
        let due = t + short_backoff(1);
        if !waits_until(&first, due) {
            wrong.push(format!("{case}: the done claim: {first:?}"));
            continue;
        }
        let check = act(&mut st, &idle(DONE_CLAIM, None), due);
        if check != checked() {
            wrong.push(format!("{case}: Cruise's act: {check:?}"));
            continue;
        }
        let wall = TurnEndReading {
            worked: Some(Duration::from_secs(2)),
            resumes_by_itself: by_itself,
            ..armed.clone()
        };
        let walled_at = if cut {
            due + Duration::from_secs(5)
        } else {
            let answered = due + Duration::from_secs(5);
            at(&mut st, &idle(DONE_CLAIM, Some(short)), answered);
            answered + MIN
        };
        // The act the wall cut counts nothing; a turn of the agent's own answered first counts.
        let at_wall = if cut { 1 } else { 2 };
        let (_, waited) = judged(&mut st, &wall, walled_at);
        if !matches!(waited, TurnEndAction::WaitUntil { .. }) || st.short_streak() != at_wall {
            wrong.push(format!(
                "{case}: PRECONDITION, at the wall {waited:?}, streak {} (want {at_wall})",
                st.short_streak()
            ));
            continue;
        }
        // the notice drawn again at the reset (`Continuing shortly`), read
        // as a turn
        let redrawn = TurnEndReading {
            worked: Some(Duration::from_secs(1)),
            ..wall.clone()
        };
        at(&mut st, &redrawn, reset - Duration::from_secs(2));
        if st.short_streak() != at_wall {
            wrong.push(format!(
                "{case}: PRECONDITION, at the notice drawn again, streak {} (want {at_wall})",
                st.short_streak()
            ));
            continue;
        }
        let (point, after) = if by_itself {
            let point = if want == 0 {
                TurnEndReading {
                    worked: Some(5 * MIN),
                    ran_tools: true,
                    ..fired_reading("Implemented stage two; stage three is next.")
                }
            } else {
                fired_reading(DONE_CLAIM)
            };
            (
                point,
                reset
                    + if want == 0 {
                        6 * MIN
                    } else {
                        Duration::from_secs(70)
                    },
            )
        } else {
            let typed_at = reset + timing.grace_for(Program::Claude);
            let quiet = TurnEndReading {
                worked: None,
                ..wall.clone()
            };
            let resumed = act(&mut st, &quiet, typed_at);
            if resumed.rule_id() != Some(RULE_LIMIT_RESUME) {
                wrong.push(format!("{case}: past the reset: {resumed:?}"));
                continue;
            }
            (
                idle(DONE_CLAIM, Some(Duration::from_secs(3))),
                typed_at + Duration::from_secs(10),
            )
        };
        let a = at(&mut st, &point, after);
        let backs_off = if want == 0 {
            a == typed(RULE_CONTINUE)
        } else {
            matches!(&a, TurnEndAction::WaitUntil {
            until,
            wait: WaitKind::BackOff { short },
            ..
        } if *until == after + short_backoff(want) && u32::from(*short) == want)
        };
        if st.short_streak() != want || !backs_off {
            wrong.push(format!(
                "{case}: streak {} (want {want}), {}",
                st.short_streak(),
                wait_past(&a, after)
            ));
            continue;
        }
        at(
            &mut st,
            &fired_reading("Ran the suites once more."),
            after + MIN,
        );
        if st.short_streak() != want + 1 {
            wrong.push(format!(
                "{case}: a later short turn of its own, the continuation's row drawn: streak {} (want {})",
                st.short_streak(),
                want + 1
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// THE SHORT-TURN STREAK COUNTS ONLY THE AGENT'S OWN SHORT ANSWERS TO ATERM (v6 §14.17.5.7).

/// The expired login's wall as Claude Code draws it (slice 12d's stand-in), which every reader
/// names as the login's.
const LOGIN_GONE: &str = "Please run /login · API Error: 401 OAuth token has expired. Please \
                          obtain a new token or refresh your existing token.";
/// An overloaded API, an outage wall the harness waits out (§14.17.5.2).
const OVERLOADED: &str = "API Error: 529 Overloaded.";
/// A short answer's own work.
const SHORT: Duration = Duration::from_secs(4);

/// A person's own short turn read at its point: their prompt `ago` before the read, inside the
/// turn ([`TurnEndReading::person`], as the hold reads it).
fn persons_turn(said: &str, ago: Duration, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        person: Some(ago),
        key: Some(ago),
        ..idle(said, worked)
    }
}

/// Where `a` is the short-turn back-off: the streak it names and how long past `from` it ends.
fn backed_off(a: &TurnEndAction, from: Instant) -> Option<(u32, Duration)> {
    match a {
        TurnEndAction::WaitUntil {
            until,
            wait: WaitKind::BackOff { short },
            ..
        } => Some((u32::from(*short), until.saturating_duration_since(from))),
        _ => None,
    }
}

/// The mode's own first act on `program` (`mode`'s word): the first prompt's short answer (a
/// driver's prompt, no person's hand in it: one short turn), then the act, Land's instruction at
/// once and Cruise's continuation past one short turn's back-off. When it was typed, or what went
/// wrong.
fn first_act(
    st: &mut TurnEndState,
    of: &dyn Fn(TurnEndReading) -> TurnEndReading,
    mode: Autopilot,
    t: Instant,
) -> Result<Instant, String> {
    let first = act(st, &of(idle(WORK_REPLY, Some(SHORT))), t);
    if mode == Autopilot::Land {
        return if first.rule_id() == Some(RULE_LAND) {
            Ok(t)
        } else {
            Err(format!("Land's instruction at the first point: {first:?}"))
        };
    }
    if backed_off(&first, t) != Some((1, short_backoff(1))) {
        return Err(format!("the first answer: {}", wait_past(&first, t)));
    }
    let due = t + short_backoff(1);
    let typed = act(st, &of(idle(WORK_REPLY, None)), due);
    if typed.rule_id() == Some(RULE_CONTINUE) {
        Ok(due)
    } else {
        Err(format!("Cruise's act past its back-off: {typed:?}"))
    }
}

/// THE ACT AN EXPIRED LOGIN CUT AND THE PERSON'S OWN TURN ADD NOTHING TO THE SHORT STREAK
/// (`TurnEndState::observe`, v6 §14.17.5.7): slice 12d's login run, under Cruise and Land, on
/// Claude Code, agy and Muse Code. The first prompt's short answer is one short turn; the mode's
/// act met by the expired login was answered by the wall, not the agent; Claude Code's `/login`
/// (aterm's command) is answered where the person leaves its dialog; and the person's own turn
/// once signed in again is no prompt of aterm's. The streak stands at one, and the mode's next act
/// waits one short turn's back-off (2 min) from the person's turn, where the old rule counted the
/// cut act and the person's turn too and waited three's (8 min, `3 short turn(s) in a row: 8 min
/// back-off` in 12d's runs). NEGATIVE CONTROLS (§14.17.5.7): counting the act a wall cut, or the
/// person's short turn, each fails every case here.
#[test]
fn the_act_an_expired_login_cut_and_the_persons_turn_add_nothing_to_the_streak() {
    let mut wrong = Vec::new();
    for mode in [Autopilot::Cruise, Autopilot::Land] {
        for program in [Program::Claude, Program::Agy, Program::Muse] {
            let tag = format!("{mode:?} {program:?}");
            let of = |r: TurnEndReading| TurnEndReading {
                program,
                ..under(mode, r)
            };
            let mut st = TurnEndState::default();
            let typed_at = match first_act(&mut st, &of, mode, t0()) {
                Ok(at) => at,
                Err(e) => {
                    wrong.push(format!("{tag}: PRECONDITION, {e}"));
                    continue;
                }
            };
            let wall = of(walled(
                WallKind::Auth,
                LOGIN_GONE,
                Some(Duration::from_secs(1)),
            ));
            let (mut now, at_wall) = judged(&mut st, &wall, typed_at + Duration::from_secs(5));
            if st.short_streak() != 1 {
                wrong.push(format!(
                    "{tag}: the act the login cut: streak {} (want 1)",
                    st.short_streak()
                ));
            }
            if at_wall.rule_id() == Some(RULE_LOGIN) {
                // The person leaves `/login`'s dialog: its point ends no turn of the agent's.
                now += 5 * MIN;
                let left = TurnEndReading {
                    login_interrupted: true,
                    ..of(persons_turn(
                        "Login interrupted",
                        Duration::from_secs(2),
                        None,
                    ))
                };
                at(&mut st, &left, now);
            }
            now += MIN;
            let ago = Duration::from_secs(5);
            at(
                &mut st,
                &of(persons_turn(NEXT_REPLY, ago, Some(Duration::from_secs(3)))),
                now,
            );
            if st.short_streak() != 1 {
                wrong.push(format!(
                    "{tag}: the person's turn: streak {} (want 1)",
                    st.short_streak()
                ));
            }
            // Past the person's grace the mode's next act waits the streak's own back-off.
            let past_grace = now + Duration::from_secs(118);
            let read = TurnEndReading {
                worked: None,
                ..of(persons_turn(
                    NEXT_REPLY,
                    ago + Duration::from_secs(118),
                    None,
                ))
            };
            let a = at(&mut st, &read, past_grace);
            if backed_off(&a, now) != Some((1, short_backoff(1))) {
                wrong.push(format!(
                    "{tag}: past the person's turn: {} (want one short turn's 2 min)",
                    wait_past(&a, now)
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// THE ACT AN OUTAGE CUT AND THE OUTAGE'S TRIES ADD NOTHING TO THE SHORT STREAK
/// (`TurnEndState::observe`, v6 §14.17.5.7): under Cruise and Land, on Claude Code, agy and Muse
/// Code, the mode's act meets an overloaded API (slice 12b's wait: no act of the mode's while it
/// stands), the blind try on the ladder's first rung is answered short once it cleared. The cut
/// act was the wall's to answer and the try is no act of the mode's (12a's and 12c's tries never
/// counted), so the streak stands at the agent's own one short answer and the mode's next act
/// waits its 2 min, where the old rule counted the cut act and waited two's 4 min. NEGATIVE
/// CONTROL (§14.17.5.7): counting the act a wall cut fails every case.
#[test]
fn the_act_an_outage_cut_and_its_tries_add_nothing_to_the_streak() {
    let timing = TurnEndTiming::default();
    let mut wrong = Vec::new();
    for mode in [Autopilot::Cruise, Autopilot::Land] {
        for program in [Program::Claude, Program::Agy, Program::Muse] {
            let tag = format!("{mode:?} {program:?}");
            let of = |r: TurnEndReading| TurnEndReading {
                program,
                ..under(mode, r)
            };
            let mut st = TurnEndState::default();
            let typed_at = match first_act(&mut st, &of, mode, t0()) {
                Ok(at) => at,
                Err(e) => {
                    wrong.push(format!("{tag}: PRECONDITION, {e}"));
                    continue;
                }
            };
            let wall = of(walled(
                WallKind::Overloaded,
                OVERLOADED,
                Some(Duration::from_secs(2)),
            ));
            let (read_at, waited) = judged(&mut st, &wall, typed_at + Duration::from_secs(5));
            let rung = read_at + timing.outage_first;
            if !outage_waits(&waited, rung, Outage::Overload, true) {
                wrong.push(format!("{tag}: PRECONDITION, at the wall: {waited:?}"));
                continue;
            }
            if st.short_streak() != 1 {
                wrong.push(format!(
                    "{tag}: the act the outage cut: streak {} (want 1)",
                    st.short_streak()
                ));
            }
            let tried = act(
                &mut st,
                &TurnEndReading {
                    worked: None,
                    ..wall.clone()
                },
                rung,
            );
            if tried != resumed(Outage::Overload, Clearing::Blind) {
                wrong.push(format!("{tag}: PRECONDITION, the rung's try: {tried:?}"));
                continue;
            }
            let answered = rung + Duration::from_secs(10);
            let a = at(&mut st, &of(idle(NEXT_REPLY, Some(SHORT))), answered);
            if st.short_streak() != 1 || backed_off(&a, answered) != Some((1, short_backoff(1))) {
                wrong.push(format!(
                    "{tag}: the try answered: streak {}, {} (want one short turn's 2 min)",
                    st.short_streak(),
                    wait_past(&a, answered)
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// THE AGENT'S OWN SHORT ANSWERS TO ATERM CLIMB THE LADDER AS THEY ALWAYS DID (v6 §14.17.5.7: the
/// spam bound is unchanged). Under Cruise, the first prompt's short answer and five continuations
/// answered short, each in new words: each back-off names the streak and runs the ladder's rung
/// (2, 4, 8, 16, 32, then 60 min). Beside it, a person's own short turn mid-ladder neither counts
/// nor clears (the streak and its rung stand), while real work clears it wherever it comes: the
/// person's long turn, and a continuation that worked with tools before an outage cut it
/// (`SupervisorTurnEnd`'s `NeverBackOffAfterRealWork`). NEGATIVE CONTROLS (§14.17.5.7): an act's
/// short answer counted as nothing stops the climb; the person's short turn counted, or their real
/// work left uncleared, or real work cut by a wall left uncleared, each fails its case.
#[test]
fn the_agents_own_short_answers_to_aterm_climb_the_ladder_as_before() {
    let words = |n: u32| {
        let step = char::from(b'a' + u8::try_from(n).unwrap_or(0));
        format!("Stage 3 of 5: still on it, step {step}.")
    };
    let mut wrong = Vec::new();
    let mut st = TurnEndState::default();
    let mut now = t0();
    let mut a = act(&mut st, &idle(&words(0), Some(SHORT)), now);
    for n in 1..=6 {
        if backed_off(&a, now) != Some((n, short_backoff(n))) {
            wrong.push(format!(
                "the ladder at {n}: {} (want {} s)",
                wait_past(&a, now),
                short_backoff(n).as_secs()
            ));
            break;
        }
        if n == 6 {
            break;
        }
        let due = now + short_backoff(n);
        let typed = act(&mut st, &idle(&words(n - 1), None), due);
        if typed.rule_id() != Some(RULE_CONTINUE) {
            wrong.push(format!("the ladder at {n}: past the back-off {typed:?}"));
            break;
        }
        now = due + Duration::from_secs(20);
        a = act(&mut st, &idle(&words(n), Some(SHORT)), now);
    }
    // Mid-ladder (three short turns), the person's own turn.
    for (case, worked, want) in [
        ("a person's short turn", SHORT, Some(3)),
        ("a person's long turn", 20 * MIN, None),
    ] {
        let mut st = TurnEndState::default();
        let mut now = t0();
        act(&mut st, &idle(&words(0), Some(SHORT)), now);
        for n in 1..=2 {
            let due = now + short_backoff(n);
            act(&mut st, &idle(&words(n - 1), None), due);
            now = due + Duration::from_secs(20);
            act(&mut st, &idle(&words(n), Some(SHORT)), now);
        }
        if st.short_streak() != 3 {
            wrong.push(format!(
                "{case}: PRECONDITION, streak {}",
                st.short_streak()
            ));
            continue;
        }
        now += MIN;
        let ago = Duration::from_secs(5);
        st.observe(&persons_turn(&words(9), ago, Some(worked)), now);
        // Decided past the person's grace, the point read again.
        let past_grace = Duration::from_secs(118);
        let reread = persons_turn(&words(9), ago + past_grace, None);
        let got = at(&mut st, &reread, now + past_grace);
        let fine = match want {
            Some(n) => {
                st.short_streak() == n && backed_off(&got, now) == Some((n, short_backoff(n)))
            }
            None => st.short_streak() == 0 && got.rule_id() == Some(RULE_CONTINUE),
        };
        if !fine {
            wrong.push(format!(
                "{case}: streak {} (want {}), {}",
                st.short_streak(),
                want.unwrap_or(0),
                wait_past(&got, now)
            ));
        }
    }
    // Real work an outage cut: the act's answer worked 5 min with tools before the wall.
    let mut st = TurnEndState::default();
    let t = t0();
    let typed_at = first_act(&mut st, &|r| r, Autopilot::Cruise, t).expect("Cruise's act");
    let cut = TurnEndReading {
        ran_tools: true,
        ..walled(WallKind::Overloaded, OVERLOADED, Some(5 * MIN))
    };
    st.observe(&cut, typed_at + 5 * MIN);
    if st.short_streak() != 0 {
        wrong.push(format!(
            "real work an outage cut: streak {} (want 0)",
            st.short_streak()
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Cruise under a wall that cuts each of its acts 5 s in (`kind`, `message`), the wall's try
/// answered short in new words each time, from a first answer that worked `start`: the streak at
/// each of the first eight continuations and the seconds between them.
fn cut_every_act(kind: WallKind, message: &str, start: Duration) -> (Vec<u32>, Vec<u64>) {
    let mut st = TurnEndState::default();
    let mut now = t0();
    let first = idle(WORK_REPLY, Some(start));
    let mut a = act(&mut st, &first, now);
    let mut last = TurnEndReading {
        worked: None,
        ..first
    };
    let (mut acts, mut streaks) = (Vec::new(), Vec::new());
    let mut n = 0u32;
    for _ in 0..400 {
        if acts.len() == 8 {
            break;
        }
        match a.clone() {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until.max(now + Duration::from_millis(1));
                a = act(&mut st, &last, now);
            }
            TurnEndAction::Type { rule_id, .. } if is_continue_rule(rule_id) => {
                acts.push(now);
                streaks.push(st.short_streak());
                n += 1;
                let wall = TurnEndReading {
                    said_tail: Some(format!("Suites are running, pass {n}.")),
                    ..walled(kind, message, Some(Duration::from_secs(2)))
                };
                now += Duration::from_secs(5);
                a = act(&mut st, &wall, now);
                last = TurnEndReading {
                    worked: None,
                    ..wall
                };
            }
            TurnEndAction::Type { rule_id, .. } if is_wall_retry(rule_id) => {
                n += 1;
                let answer = idle(
                    &format!("Checked again, step {n}: the lexer's tests pass."),
                    Some(SHORT),
                );
                now += Duration::from_secs(10);
                a = act(&mut st, &answer, now);
                last = TurnEndReading {
                    worked: None,
                    ..answer
                };
            }
            _ => break,
        }
    }
    let gaps = acts
        .windows(2)
        .map(|w| w[1].saturating_duration_since(w[0]).as_secs())
        .collect();
    (streaks, gaps)
}

/// A WALL THAT CUTS EVERY ACT STILL CLIMBS THE LADDER (`TurnEndState::observe`, v6 §14.17.5.7:
/// the wall's one pass per rung). Under Cruise, each continuation meets the wall 5 s in (an
/// overloaded API, a 429 that names its retry, a 500), the wall's try clears it and the agent
/// answers that try short in new words (no act of Cruise's, so no count). The first cut act is
/// the wall's pass and adds nothing; each one past it, with no answer of the agent's own or real
/// work between, counts, so the streak climbs one an act and the gap between Cruise's acts grows
/// to the ladder's hour, as before the pass, rather than Cruise prompting at one pace forever
/// (§14.17: never spam the agent). Beside it, the pass comes back once the agent's own short
/// answer to an act of Cruise's moved the streak: the next cut act adds nothing again.
/// NEGATIVE CONTROLS (§14.17.5.7): every cut act left uncounted keeps the streak flat and the
/// gap fixed; the pass never coming back counts the cut act after the agent's own answer.
#[test]
fn a_wall_that_cuts_every_act_still_climbs_the_ladder() {
    let mut wrong = Vec::new();
    for (name, kind, message) in [
        ("an overload", WallKind::Overloaded, OVERLOADED),
        (
            "a 429 that names its retry",
            WallKind::ApiError {
                code: Some(429),
                retryable: true,
                cause: ApiCause::Server,
            },
            "API Error: 429 Too Many Requests. Please try again in 5s.",
        ),
        (
            "a 500",
            WallKind::ApiError {
                code: Some(500),
                retryable: true,
                cause: ApiCause::Server,
            },
            "API Error: 500 Internal server error.",
        ),
    ] {
        for (start, work, first) in [
            ("after real work", 20 * MIN, 0),
            ("after a short answer", SHORT, 1),
        ] {
            let (streaks, gaps) = cut_every_act(kind, message, work);
            let want: Vec<u32> = (0..8u32).map(|i| first + i.saturating_sub(1)).collect();
            let cap = TurnEndTiming::default().short_backoff_max.as_secs();
            let climbs = gaps.windows(2).skip(1).all(|w| w[1] > w[0] || w[0] >= cap);
            if streaks != want || !climbs {
                wrong.push(format!(
                    "{name} {start}: streaks {streaks:?} (want {want:?}), gaps {gaps:?} s"
                ));
            }
        }
    }
    // The pass comes back once the agent's own short answer moved the streak.
    let mut st = TurnEndState::default();
    let typed_at = first_act(&mut st, &|r| r, Autopilot::Cruise, t0()).expect("Cruise's act");
    let wall = walled(
        WallKind::Overloaded,
        OVERLOADED,
        Some(Duration::from_secs(2)),
    );
    let mut now = typed_at;
    for (step, want) in [
        ("the first cut", 1),
        ("the cut after the agent's answer", 2),
    ] {
        let (read_at, waited) = judged(&mut st, &wall, now + Duration::from_secs(5));
        let TurnEndAction::WaitUntil { until: rung, .. } = waited else {
            wrong.push(format!("{step}: PRECONDITION, at the wall {waited:?}"));
            break;
        };
        let tried = act(
            &mut st,
            &TurnEndReading {
                worked: None,
                ..wall.clone()
            },
            rung.max(read_at),
        );
        if !tried.rule_id().is_some_and(is_wall_retry) {
            wrong.push(format!("{step}: PRECONDITION, the try {tried:?}"));
            break;
        }
        let answered = rung.max(read_at) + Duration::from_secs(10);
        let a = at(
            &mut st,
            &idle(&format!("{NEXT_REPLY} ({step})"), Some(SHORT)),
            answered,
        );
        if st.short_streak() != want {
            wrong.push(format!(
                "{step}: streak {} (want {want})",
                st.short_streak()
            ));
            break;
        }
        // Cruise's act past the back-off, answered short by the agent: its own answer.
        let due = match a {
            TurnEndAction::WaitUntil { until, .. } => until,
            _ => answered,
        };
        let typed = act(&mut st, &idle(&format!("{NEXT_REPLY} ({step})"), None), due);
        if typed.rule_id() != Some(RULE_CONTINUE) {
            wrong.push(format!("{step}: PRECONDITION, past the back-off {typed:?}"));
            break;
        }
        if want == 1 {
            let mine = due + Duration::from_secs(20);
            at(
                &mut st,
                &idle(&format!("{WORK_REPLY} (own)"), Some(SHORT)),
                mine,
            );
            let due = mine + short_backoff(2);
            let typed = act(&mut st, &idle(&format!("{WORK_REPLY} (own)"), None), due);
            if st.short_streak() != 2 || typed.rule_id() != Some(RULE_CONTINUE) {
                wrong.push(format!(
                    "the agent's own answer: streak {}, then {typed:?}",
                    st.short_streak()
                ));
                break;
            }
            now = due;
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A LOOP AFTER A RESTART COUNTS THE VENDOR'S CONTINUATION OF A CUT ACT AS THE LOOP BEFORE IT
/// WOULD (`TurnEndState::seed_wall_pass`, v6 §14.17.5.7): under Cruise, Claude Code's done claim
/// (one short turn), Cruise's done check met by its session limit with its auto-continue armed,
/// and Claude Code going on by itself past the reset, its turn short. The loop that never
/// restarts ends at one. A successor seeded from the act's row (its answer unread) reads the cut
/// itself; one seeded from the wall's point row reads the wall's pass from the books
/// ([`approvals::point_reason`], [`approvals::short_streak_in`]); both end at one too. NEGATIVE
/// CONTROLS: the seeded yield that leaves the cut act unowed, or the books that drop the pass,
/// count the continuation (two).
#[test]
fn a_successor_counts_the_vendors_continuation_of_a_cut_act_as_the_loop_before() {
    use crate::supervise::approvals::{self, Carried, Outcome, Row};
    let t = t0();
    let reset = t + 10 * MIN;
    let armed = under(
        Autopilot::Cruise,
        captured(AUTO_CONTINUE_ARMED, Some(reset), Duration::ZERO),
    );
    let fired = TurnEndReading {
        worked: Some(Duration::from_secs(3)),
        ..under(
            Autopilot::Cruise,
            captured_rows(&replied_after_the_reset(DONE_CLAIM, true)),
        )
    };
    let wall = TurnEndReading {
        worked: Some(Duration::from_secs(2)),
        resumes_by_itself: true,
        ..armed
    };
    let quiet_wall = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    let mut st = TurnEndState::default();
    act(&mut st, &idle(DONE_CLAIM, Some(SHORT)), t);
    let due = t + short_backoff(1);
    let check = act(&mut st, &idle(DONE_CLAIM, None), due);
    assert_eq!(check, checked(), "PRECONDITION");
    let at_act = (st.short_streak(), st.wall_pass());
    let walled_at = due + Duration::from_secs(5);
    judged(&mut st, &wall, walled_at);
    // The successor seeded from the act's row, its answer unread.
    let mut yielded = TurnEndState::default();
    yielded.seed_short(at_act.0);
    yielded.seed_wall_pass(at_act.1);
    yielded.seed_short_program(Program::Claude);
    yielded.seed_worked();
    yielded.seed_yield(RULE_DONE_CHECK);
    judged(&mut yielded, &wall, walled_at);
    // The successor seeded from the wall's point row, read back from the books.
    let reason = approvals::point_reason(
        Carried {
            pass: st.wall_pass(),
            ..Carried::streak(st.short_streak())
        },
        false,
    );
    let row = Row {
        rule_id: approvals::RULE_POINT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 1,
    };
    let books = format!("{}\n", row.to_json(1_000, Some("s-1")));
    let seed = approvals::short_streak_in(&books, Some("s-1")).expect("the point's seed");
    let mut pointed = TurnEndState::default();
    pointed.seed_short(seed.short);
    pointed.seed_wall_pass(seed.pass);
    pointed.seed_short_program(Program::Claude);
    pointed.seed_worked();
    pointed.seed_point(MIN, walled_at + MIN);
    let mut got = Vec::new();
    for (name, s) in [
        ("never restarted", &mut st),
        ("seeded from the act", &mut yielded),
        ("seeded from the wall's point", &mut pointed),
    ] {
        at(s, &quiet_wall, walled_at + MIN);
        at(s, &fired, reset + Duration::from_secs(70));
        got.push((name, s.short_streak()));
    }
    assert!(got.iter().all(|(_, n)| *n == 1), "{got:?}");
}
