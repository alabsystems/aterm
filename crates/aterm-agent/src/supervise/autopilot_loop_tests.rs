// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// AUTOPILOT in the loop, over the scripted server: the session's `autopilot`
// word read by key off the bare `meta` reply (else the table's `[harness]
// autopilot`), Land typing the land instruction where the continuation would
// go, and Off parking the loop — nothing pressed, typed or escalated — until
// the word changes. Each behaviour has its negative control. Included from
// `run_tests.rs` (`mod autopilot_loop`), so the `Mock` and the helpers there
// are in scope.

use crate::supervise::journal::{AGAIN_ADOPTED, AGAIN_RETRY, AGAIN_UNPARKED};
use crate::supervise::policy::turn_end::{
    DONE_CHECK, LAND, RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LAND_NUDGE, RULE_LEFT,
};

/// A turn that ended after 3 minutes of work (the done row's measure), the
/// worker's last words `said`, at an empty composer in bypass mode.
fn worked(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// `policy` as the in-GUI host runs it.
fn under(policy: SupervisorConfig) -> SuperviseOpts {
    SuperviseOpts { policy, ..auto(30) }
}

/// The session's word `land`: the point is decided at Cruise first, the word
/// read (one bare `meta`, before the `status` an act reads) where Cruise
/// would act, and the land instruction typed where `keep going` would be;
/// the worker's clean defense (`Nothing left: …`) lands the task, said once
/// (`DONE … rule=land@v1`). NEGATIVE CONTROL: a host whose `meta` names no
/// word (an older build) is asked nothing more, and the table's Cruise
/// continues.
#[test]
fn the_sessions_land_word_types_the_land_instruction_and_a_clean_defense_lands() {
    let (dir, journal) = journal_file("autopilot-land");
    let mut m = Mock::new(vec![
        busy_screen(),
        worked("Fixed the parser; the suite is green."),
        busy_screen(),
        worked("Nothing left: all of it is merged into the remote main branch."),
    ]);
    m.history_served();
    m.autopilot = Some("land");
    m.turn_releases = Some(1);
    m.vanish_after = Some(4);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..under(owners_policy())
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let head = format!("CONTINUED seq=102 rule={RULE_LAND} ");
    let typed: Vec<&String> = lines.iter().filter(|l| l.starts_with(&head)).collect();
    assert_eq!(typed.len(), 1, "{lines:#?}");
    assert!(LAND.starts_with(&typed[0][head.len()..]), "{lines:#?}");
    assert_eq!(
        count(&m, "turn"),
        1,
        "the instruction alone: {:#?}",
        m.requests
    );
    let turn = continue_request(LAND);
    let at = m
        .requests
        .iter()
        .position(|r| *r == turn)
        .expect("the land instruction");
    assert_eq!(
        m.requests[at - 2..at],
        ["meta".to_string(), "status".to_string()],
        "the word read before the act's status: {:#?}",
        m.requests
    );
    let done: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("DONE "))
        .collect();
    assert_eq!(done.len(), 1, "{done:#?}");
    assert!(
        done[0].starts_with(&format!("DONE seq=104 rule={RULE_LAND} ")),
        "{done:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Negative control: no word on this host — the table's Cruise, and no
    // `meta` read for it.
    let mut m = Mock::new(vec![
        busy_screen(),
        worked("Fixed the parser; the suite is green."),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &under(owners_policy()));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
        )),
        "{lines:#?}"
    );
    let turn = continue_request("keep going");
    let at = m
        .requests
        .iter()
        .position(|r| *r == turn)
        .expect("the turn");
    assert_eq!(m.requests[at - 1], "status");
    assert_eq!(
        count(&m, "meta"),
        1,
        "the reconcile's read alone: {:#?}",
        m.requests
    );
}

/// A PERSON'S OWN EXCHANGE UNDER THE SESSION'S LAND WORD: a greeting the
/// person's keystroke opened, answered in seconds, gets no land instruction
/// — the turn is theirs under Land as under Cruise — and the journal says
/// once why (`LEFT … rule=left-to-person@v1`), never Land's `DONE`. The
/// person's grace is off, so nothing but the rule holds the act. NEGATIVE
/// CONTROL: the same turn with no person's stamp gets the land instruction
/// after the back-off.
#[test]
fn a_persons_greeting_under_land_is_left_to_them() {
    let greeting = "Hi! What would you like me to work on?";
    let mut quick = rows(&[&format!("⏺ {greeting}"), ""]);
    quick.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let timing = |s: &mut Watched<'_, '_>| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    };
    let policy = SupervisorConfig {
        human_grace_s: 0,
        ..owners_policy()
    };
    let (dir, journal) = journal_file("autopilot-land-exchange");
    let mut m = Mock::new(vec![busy_screen(), quick.clone()]);
    m.autopilot = Some("land");
    m.turn_releases = Some(1);
    m.vanish_after = Some(8);
    m.human = vec!["2000"];
    let opts = SuperviseOpts {
        policy: policy.clone(),
        journal: Some(journal.clone()),
        ..auto(1)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, timing);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    let records: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let left: Vec<&String> = records.iter().filter(|l| l.starts_with("LEFT ")).collect();
    assert_eq!(left.len(), 1, "{records:#?}");
    assert!(left[0].contains(&format!("rule={RULE_LEFT} ")), "{left:#?}");
    assert!(
        !records.iter().any(|l| l.starts_with("DONE ")),
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Negative control: nobody's stamp — the land instruction.
    let mut m = Mock::new(vec![busy_screen(), quick]);
    m.autopilot = Some("land");
    m.turn_releases = Some(1);
    m.vanish_after = Some(8);
    let opts = SuperviseOpts { policy, ..auto(1) };
    let (lines, _) = watch_lines_with(&mut m, &opts, timing);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
        "{lines:#?}"
    );
}

/// The window's host's word on how it started the loop
/// ([`IdleHost::lifted_off`]): as the session's Off lifted, or not.
#[derive(Debug)]
struct Lifted(bool);

impl IdleHost for Lifted {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn lifted_off(&self) -> bool {
        self.0
    }
}

/// A screen whose last turn the message `head` (its rows as drawn) opened
/// and the agent answered `said`, its done row `done` (none: `None`), at an
/// empty composer in bypass mode.
fn answered(head: &[&str], said: &str, done: Option<&str>) -> Vec<String> {
    let mut r = rows(head);
    r.extend(rows(&["", &format!("⏺ {said}"), ""]));
    if let Some(done) = done {
        r.extend(rows(&[&format!("✻ Worked for {done}"), ""]));
    }
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// Acted on, never left to the person: a turn typed under `rule`, and no
/// `LEFT` journaled.
fn acted_on(case: &str, rule: &str, (turns, said, lines): &(usize, Vec<String>, Vec<String>)) {
    assert!(*turns >= 1, "{case}: {lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains(&format!("rule={rule} "))),
        "{case}: {lines:#?}"
    );
    assert!(
        !said.iter().any(|l| l.starts_with("LEFT ")),
        "{case}: {said:#?}"
    );
}

/// OFF BACK TO CRUISE: THE POINT THE NEW LOOP FINDS IS ACTED ON, NEVER LEFT
/// ON A GESTURE (the review of 7c7771267, findings 1 and 2, over the loop).
/// Under Off the window's host stops the worker; the word turns back and the
/// host starts a new loop ([`IdleHost::lifted_off`]) that attaches to an idle
/// point no read of its saw run, the person's last gesture a scroll 10 s
/// back. The turn there was the harness's own act answered while Off stood —
/// `❯ keep going` with a done row of 1m 12s or none, the done check answered
/// `Not finished…`, the land instruction (the chip's Land → Off → Cruise)
/// answered in 50 s — or the person's check-in on the run, `❯ status?`, or
/// their greeting `❯ hi`. Each is backed off, then acted on: the
/// continuation, or the done check over the greeting. FAILS WITH 7c7771267
/// (its first-point arm, the message drawn above a short reply): every one
/// was `LEFT … rule=left-to-person@v1`, nothing typed, Cruise stalled with
/// nothing escalated. CONTROLS, acted on in both: a loop started for
/// another reason (its ledger carries the task), and a lifted loop with
/// nobody's stamp.
#[test]
fn a_loop_started_as_off_lifted_acts_on_the_point_it_finds() {
    // An act's text as Claude Code wraps it under its `❯`: two rows, split
    // at a space near the middle.
    let wrapped = |text: &str| {
        let (first, rest) = text.split_at(text[..text.len() / 2].rfind(' ').expect("a space"));
        [format!("❯ {first}"), format!("  {}", rest.trim_start())]
    };
    let land_rows = wrapped(LAND);
    let land_rows: Vec<&str> = land_rows.iter().map(String::as_str).collect();
    let check_rows = wrapped(DONE_CHECK);
    let check_rows: Vec<&str> = check_rows.iter().map(String::as_str).collect();
    // A case: its name, its screen, the act the stopped loop typed (its rule
    // and text), and the rule of the act the new loop types.
    type Case<'a> = (&'a str, Vec<String>, (&'a str, &'a str), &'a str);
    let cases: Vec<Case<'_>> = vec![
        (
            "1a: keep going answered, done in 1m 12s",
            answered(
                &["❯ keep going"],
                "Wired the lexer into the parser; the docs are next.",
                Some("1m 12s"),
            ),
            (RULE_CONTINUE, "keep going"),
            RULE_CONTINUE,
        ),
        (
            "1b: keep going answered, no done row",
            answered(
                &["❯ keep going"],
                "Wired the lexer into the parser; the docs are next.",
                None,
            ),
            (RULE_CONTINUE, "keep going"),
            RULE_CONTINUE,
        ),
        (
            "1c: the done check answered",
            answered(
                &check_rows,
                "Not finished: the docs are out of date. Updating them next.",
                Some("40s"),
            ),
            (RULE_DONE_CHECK, DONE_CHECK),
            RULE_CONTINUE,
        ),
        (
            "1d: the land instruction answered in 50 s",
            answered(
                &land_rows,
                "Left: the docs and the release notes.",
                Some("50s"),
            ),
            (RULE_LAND, LAND),
            RULE_CONTINUE,
        ),
        (
            "a check-in on the carried run",
            answered(
                &["❯ status?"],
                "Tests are green; the docs are next.",
                Some("5s"),
            ),
            (RULE_CONTINUE, "keep going"),
            RULE_CONTINUE,
        ),
        (
            "a greeting under Off",
            answered(
                &["❯ hi"],
                "Hi! What would you like me to work on?",
                Some("2s"),
            ),
            (RULE_CONTINUE, "keep going"),
            RULE_DONE_CHECK,
        ),
    ];
    for (case, screen, typed, rule) in &cases {
        let ago = Some((typed.0, typed.1, Duration::from_secs(80)));
        let run = run_lifted(
            vec![screen.clone()],
            Some("10000"),
            Some(true),
            ago,
            "autopilot-lifted-acts",
        );
        acted_on(&format!("lifted: {case}"), rule, &run);
        // CONTROLS: started for another reason; nobody's stamp.
        let run = run_lifted(
            vec![screen.clone()],
            Some("10000"),
            Some(false),
            ago,
            "autopilot-restarted-acts",
        );
        acted_on(&format!("restarted: {case}"), rule, &run);
        let run = run_lifted(
            vec![screen.clone()],
            None,
            Some(true),
            ago,
            "autopilot-lifted-nobody",
        );
        acted_on(&format!("nobody: {case}"), rule, &run);
    }
}

/// THE SAME UNDER THE ENGINE'S OWN PARK (no host stops it: the table's `off`
/// under a loop with no host, a word read before the host acts): the loop parks
/// at a point Cruise would continue, the person says hi under Off and the
/// agent answers at once, the word turns back — and the point read afresh,
/// never seen busy, is acted on whether or not the person's keystroke is in
/// it ([`TurnEndState::park_off`] ends the carry, and judges nothing).
/// FAILS WITH 85d489954's first-point arm: the keystroke made the point
/// theirs (`LEFT`), nothing typed. CONTROL: nobody's stamp, a turn typed.
#[test]
fn an_off_park_then_cruise_acts_on_the_point_it_never_saw_run() {
    let greeting = "Hi! What would you like me to work on?";
    let quick = answered(&["❯ hi"], greeting, None);
    let run = |hand: bool, tag: &str| {
        let (dir, journal) = journal_file(tag);
        let mut m = Mock::new(vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
            quick.clone(),
        ]);
        m.autopilot = Some("cruise");
        // The decision at the worked point reads `off` and parks; the
        // park's next read, `cruise`.
        m.autopilot_at = vec![(2, Some("off")), (3, Some("cruise"))];
        // The person's keystroke as the answer's screen is read (`0` ms):
        // after the point the park was decided at, whatever the clock.
        if hand {
            m.human_on.insert(2, "0");
        }
        m.vanish_after = Some(20);
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            journal: Some(journal.clone()),
            ..auto(1)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (count(&m, "turn"), said, lines, m.reads.clone())
    };
    for (hand, tag) in [
        (true, "autopilot-park-greeting"),
        (false, "autopilot-park-nobody"),
    ] {
        let (turns, said, lines, reads) = run(hand, tag);
        assert!(
            said.iter().any(|l| l.starts_with("UNPARKED ")),
            "{tag}: {said:#?}"
        );
        assert!(turns >= 1, "{tag}: {lines:#?} {said:#?} {reads:?}");
        assert!(
            !said.iter().any(|l| l.starts_with("LEFT ")),
            "{tag}: {said:#?}"
        );
    }
}

/// A loop's run over `screens` under Cruise with every read's person stamp
/// `human` (none: `None`), the host `lifted` (`None`: no host), and an act
/// the stopped loop typed — its rule, its text, how long before (none: no
/// ledger) — at test speed: what was typed, journaled and said.
fn run_lifted(
    screens: Vec<Vec<String>>,
    human: Option<&'static str>,
    lifted: Option<bool>,
    typed: Option<(&str, &str, Duration)>,
    tag: &str,
) -> (usize, Vec<String>, Vec<String>) {
    let (dir, journal) = journal_file(tag);
    let ledger = dir.join("ledger.jsonl");
    if let Some((rule_id, command, ago)) = typed {
        let row = crate::supervise::approvals::Row {
            rule_id,
            outcome: crate::supervise::approvals::Outcome::Typed,
            command,
            reason: "-",
            box_seq: 1,
        };
        let at = crate::supervise::journal::unix_ms() - i64::try_from(ago.as_millis()).unwrap();
        std::fs::write(&ledger, row.to_json(at, None) + "\n").expect("ledger");
    }
    let mut m = Mock::new(screens);
    m.vanish_after = Some(20);
    m.stall_sleep = Some(Duration::from_millis(20));
    // Each case is judged by the act the new loop types: the run ends at it,
    // and a loop that leaves the point types nothing and runs to its budget —
    // a minute, the hang detector, never a latency budget: on a loaded host
    // the back-offs and reads before the act outrun a second.
    m.exit_when = Some(|m| m.turns > 0);
    m.human = human.into_iter().collect();
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
        },
        journal: Some(journal.clone()),
        idle_host: lifted.map(|l| Arc::new(Lifted(l)) as Arc<dyn IdleHost>),
        ..auto(aterm_spec::wait::MUST_HAPPEN.as_secs())
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        if typed.is_some() {
            s.set_approval_ledger(Some(ledger.clone()));
        }
    });
    let said = journaled(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    (count(&m, "turn"), said, lines)
}

/// Continued, never left to the person: a `keep going` typed under the
/// continue rule, and no `LEFT` journaled.
fn continued(case: &str, (turns, said, lines): &(usize, Vec<String>, Vec<String>)) {
    assert!(*turns >= 1, "{case}: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_CONTINUE} keep going"))),
        "{case}: {lines:#?}"
    );
    assert!(
        !said.iter().any(|l| l.starts_with("LEFT ")),
        "{case}: {said:#?}"
    );
}

/// A SCROLL OVER A LONG TURN NOBODY SAW RUN IS NO EXCHANGE OF THE PERSON'S
/// (the review of 85d489954, finding 1, over the loop): the loop attaches to
/// a point the screen draws as 23 minutes of work (`✻ Worked for 23m 2s`),
/// and the person's last gesture is a wheel scroll 10 s ago. Started as Off
/// lifted over the stopped loop's act 5 minutes back, or fresh with no act in
/// the window (a policy hand-over, a fault restart, a new window): either
/// continues the work after the back-off — the turn's message scrolled away
/// or still drawn above it. FAILS WITH 85d489954's first-point arm: every
/// one was `LEFT … rule=left-to-person@v1`, nothing typed for good. A turn
/// no read saw run is never left to anyone
/// (`a_loop_started_as_off_lifted_acts_on_the_point_it_finds`).
#[test]
fn a_scroll_over_a_long_turn_nobody_saw_run_is_no_exchange_of_the_persons() {
    let long = |head: bool| {
        let mut r = if head {
            rows(&["❯ refactor the lexer", ""])
        } else {
            Vec::new()
        };
        r.extend(rows(&[
            "⏺ Refactored the lexer across 14 files; the parser is next.",
            "",
            "✻ Worked for 23m 2s · done 4:24 PM",
            "",
        ]));
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    for head in [false, true] {
        let case = format!("lifted, message drawn: {head}");
        let run = run_lifted(
            vec![long(head)],
            Some("10000"),
            Some(true),
            Some((RULE_CONTINUE, "keep going", 5 * MIN_LOOP)),
            "autopilot-f1-lifted",
        );
        continued(&case, &run);
        let case = format!("fresh, message drawn: {head}");
        let run = run_lifted(
            vec![long(head)],
            Some("10000"),
            None,
            None,
            "autopilot-f1-fresh",
        );
        continued(&case, &run);
    }
}

/// A SCROLL OVER A DAY-LONG TURN NOBODY SAW RUN IS CONTINUED (the review of
/// 7c7771267, its low finding 3, over the loop): a fresh loop with no act in
/// the window (a hand-over, a fault restart) or one started as Off lifted
/// attaches to `❯ run the soak test` drawn above a 26-hour soak's report,
/// its done row `✻ Sautéed for 1d 2h 3m`, the person's scroll 10 s back.
/// Continued after the back-off: a turn no read saw run is never left to
/// anyone, whatever its done row says or this build reads of it
/// (`done_row_work` reads no day; a wall's day-long done row counts the
/// wait, and the usage wall's conformance holds it unread). FAILS WITH
/// 7c7771267: the unread day was no done row, the drawn message made it a
/// quick exchange, `LEFT … rule=left-to-person@v1`. CONTROL: `23h 2m`,
/// continued there too.
#[test]
fn a_scroll_over_a_day_long_turn_nobody_saw_run_is_continued() {
    let soak = |span: &str| {
        let mut r = rows(&[
            "❯ run the soak test",
            "",
            "⏺ Bash(./soak.sh --hours 26)",
            "  ⎿  soak: 26h of traffic, 0 errors",
            "",
            "⏺ The soak passed; next I'll write up the numbers.",
            "",
            &format!("✻ Sautéed for {span} · done 12:19 PM"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    for span in ["1d 2h 3m", "23h 2m"] {
        let run = run_lifted(
            vec![soak(span)],
            Some("10000"),
            None,
            None,
            "autopilot-day-fresh",
        );
        continued(&format!("fresh, {span}"), &run);
        let run = run_lifted(
            vec![soak(span)],
            Some("10000"),
            Some(true),
            Some((RULE_CONTINUE, "keep going", 30 * 60 * MIN_LOOP)),
            "autopilot-day-lifted",
        );
        continued(&format!("lifted, {span}"), &run);
    }
}

/// THE POINT OFF STOOD ON, READ AGAIN AS THE WORD TURNS BACK, IS CONTINUED
/// AS BEFORE THE PARK (the review of 85d489954, finding 2, over the loop):
/// a turn seen busy ends at a point the decision there reads Off at; the
/// person scrolls under Off, and the word turns back to Cruise with the
/// same point on the screen. Continued — whether its done row says 3
/// minutes or none is drawn and its message is (`❯ land steps 3 and 4`).
/// FAILS WITH 85d489954's first-point arm: `LEFT … rule=left-to-person@v1`,
/// nothing typed. A greeting the person sent under Off is acted on too
/// (`an_off_park_then_cruise_acts_on_the_point_it_never_saw_run`).
#[test]
fn the_point_off_stood_on_read_again_is_continued_as_before_the_park() {
    let said = "Step 3 landed; step 4 is next.";
    let mut drawn = rows(&["❯ land steps 3 and 4", "", &format!("⏺ {said}"), ""]);
    drawn.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    for (case, point) in [("3 minutes", worked(said)), ("no done row", drawn)] {
        let (dir, journal) = journal_file("autopilot-f2-park");
        let mut m = Mock::new(vec![busy_screen(), point.clone(), point]);
        m.autopilot = Some("cruise");
        m.autopilot_at = vec![(2, Some("off")), (3, Some("cruise"))];
        // The person's scroll under Off, read on the point drawn again.
        m.human_on.insert(2, "0");
        m.vanish_after = Some(20);
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            journal: Some(journal.clone()),
            ..auto(1)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            said.iter().any(|l| l.starts_with("UNPARKED ")),
            "{case}: {said:#?}"
        );
        continued(case, &(count(&m, "turn"), said, lines));
    }
}

/// A LOOP STARTED AS OFF LIFTED, ATTACHED MID-TURN, CONTINUES THE HARNESS'S
/// RUN (the review of 85d489954, finding 3, over the loop): the stopped
/// loop typed `keep going` 4 minutes ago and the agent is still on it as the
/// host starts the new loop (the chip's Land → Off → Cruise); its first read
/// is the turn live, its end draws no done row, and the person's last
/// gesture is a scroll 10 s before it. Continued. FAILS WITHOUT THE FIX: the
/// lifted start dropped the carry, and the scroll made the turn's tail a
/// person's exchange: `LEFT … rule=left-to-person@v1`, nothing typed.
#[test]
fn a_loop_started_as_off_lifted_mid_turn_continues_the_harness_run() {
    let mut tail = rows(&["⏺ Committed step 4; step 5 is next.", ""]);
    tail.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let run = run_lifted(
        vec![busy_screen(), tail],
        Some("10000"),
        Some(true),
        Some((RULE_CONTINUE, "keep going", 4 * MIN_LOOP)),
        "autopilot-f3-lifted",
    );
    continued("lifted mid-turn", &run);
}

/// A minute, for the ledger's ages.
const MIN_LOOP: Duration = Duration::from_secs(60);

/// The table's default decides a session with no word: `autopilot = "land"`
/// lands it with no word read (the host keeps none); a session's `cruise`
/// under the table's `off` is the owner opting that session in, and is
/// continued.
#[test]
fn the_table_decides_a_session_with_no_word_and_a_word_decides_over_the_table() {
    let land = SupervisorConfig {
        autopilot: Autopilot::Land,
        ..owners_policy()
    };
    let mut m = Mock::new(vec![
        busy_screen(),
        worked("Fixed the parser; the suite is green."),
    ]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &under(land));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(&format!("CONTINUED seq=102 rule={RULE_LAND} "))),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta"), 1, "{:#?}", m.requests);

    let off = SupervisorConfig {
        autopilot: Autopilot::Off,
        ..owners_policy()
    };
    let mut m = Mock::new(vec![
        busy_screen(),
        worked("Fixed the parser; the suite is green."),
    ]);
    m.autopilot = Some("cruise");
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &under(off));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
        )),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("PARKED")), "{lines:#?}");
}

/// WHO SET IT IS READ PAST (aterm-gui's `meta` since 2026-10-19, design
/// `docs/DESIGN-harness-autopilot-2026-10-15.md` §11.7.2): a bare `meta`
/// whose tail names who set the word and when after `land_at=`
/// (`autopilot_by=human:menu autopilot_since=1792177200`) gives the loop the
/// same word as a reply without them, set or unset — the reader keys by
/// name, and `autopilot_by=` is not `autopilot=`. NEGATIVE CONTROL: the
/// setter spelled under the word's own key, ahead of it, is read as the
/// word, and no word parses from it — the table's decides.
#[test]
fn the_autopilot_word_is_read_past_who_set_it() {
    let land = SupervisorConfig {
        autopilot: Autopilot::Land,
        ..SupervisorConfig::default()
    };
    let opts = under(land);
    let head = "OK title=- user_title=- description=- icon=- role=- attention=- cwd=- state=idle \
                attention_owner=- attention_owners=0 attention_kinds=- supervisor=- \
                supervisor_proven=0 questions=- agent_cwd=- trusted_cwd=-";
    let read = |tail: &str| {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.note_meta(&format!("{head} {tail}\n"));
        s.autopilot(&opts)
    };
    for (word, want) in [
        ("off", Autopilot::Off),
        ("cruise", Autopilot::Cruise),
        ("-", Autopilot::Land),
    ] {
        let bare = read(&format!("autopilot={word} land_at=-"));
        assert_eq!(bare, want, "{word}");
        for by in ["human:menu", "ctl", "aterm", "restored", "-"] {
            assert_eq!(
                read(&format!(
                    "autopilot={word} land_at=- autopilot_by={by} autopilot_since=1792177200"
                )),
                want,
                "{word} by {by}"
            );
        }
    }
    // NEGATIVE CONTROL: the setter under the word's key, read first.
    assert_eq!(
        read("autopilot=human:menu autopilot=off land_at=-"),
        Autopilot::Land,
        "the first `autopilot=` is the word"
    );
}

/// The journal's lines of the run that wrote `journal`.
fn journaled(journal: &Path) -> Vec<String> {
    journal_records(journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect()
}

/// OFF: the loop parks — nothing typed, pressed, escalated or reported —
/// reading the word again at each step (`PARKED …` in the journal, as a hold
/// is `HALTED …`), and once the word is `cruise` it says so and decides the
/// point afresh: the continuation goes. Under the table's `off` on a host
/// that keeps no word, nothing is asked at all and the park lasts until the
/// loop ends. NEGATIVE CONTROL: the same script with the word `cruise`
/// throughout is continued and never parked.
#[test]
fn an_autopilot_off_parks_the_loop_until_the_word_changes() {
    let script = || {
        vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
        ]
    };
    let run = |m: &mut Mock, policy: SupervisorConfig, tag: &str| {
        let (dir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..under(policy)
        };
        let (lines, _) = watch_lines(m, &opts);
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (lines, said)
    };
    // Off throughout: parked, the word read at each step.
    let mut m = Mock::new(script());
    m.autopilot = Some("off");
    m.vanish_after = Some(3);
    let (lines, said) = run(&mut m, owners_policy(), "autopilot-off");
    assert!(
        said.iter()
            .any(|l| l.starts_with("PARKED seq=102 autopilot=off")),
        "{said:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("EVENT ") || l.starts_with("CONTINUED")),
        "nothing reported or typed: {lines:#?}"
    );
    assert_eq!(count(&m, "turn") + count(&m, "key"), 0, "{:#?}", m.requests);
    assert_eq!(m.attention, None, "nothing escalated");
    assert!(
        m.metas >= 3,
        "the word read at each step: {:#?}",
        m.requests
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("EXIT session gone")),
        "{lines:#?}"
    );

    // Off, then cruise at the third read: unparked, and continued.
    let mut m = Mock::new(script());
    m.autopilot = Some("off");
    m.autopilot_at = vec![(3, Some("cruise"))];
    m.vanish_after = Some(4);
    let (lines, said) = run(&mut m, owners_policy(), "autopilot-back");
    let parked = said
        .iter()
        .position(|l| l.starts_with("PARKED seq=102"))
        .expect("parked");
    let unparked = said
        .iter()
        .position(|l| l.starts_with("UNPARKED seq=102 autopilot=cruise after "))
        .expect("unparked");
    // The point read afresh after the park (the screen's seq moved on).
    let kept_going = |l: &String| {
        l.starts_with("CONTINUED seq=") && l.ends_with(&format!(" rule={RULE_CONTINUE} keep going"))
    };
    let continued = said.iter().position(kept_going).expect("continued");
    assert!(parked < unparked && unparked < continued, "{said:#?}");
    assert!(lines.iter().any(kept_going), "{lines:#?}");
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);

    // The table's off on a host that keeps no word: parked, nothing asked.
    let off = SupervisorConfig {
        autopilot: Autopilot::Off,
        ..owners_policy()
    };
    let mut m = Mock::new(script());
    m.vanish_after = Some(3);
    let (_, said) = run(&mut m, off, "autopilot-table-off");
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=102")),
        "{said:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_eq!(
        count(&m, "meta"),
        1,
        "the reconcile's read alone: {:#?}",
        m.requests
    );

    // Negative control: cruise throughout.
    let mut m = Mock::new(script());
    m.autopilot = Some("cruise");
    m.vanish_after = Some(2);
    let (_, said) = run(&mut m, owners_policy(), "autopilot-cruise");
    assert!(!said.iter().any(|l| l.contains("PARKED")), "{said:#?}");
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
}

/// OFF is hands off for the approval policy too: a box the word turned off
/// at — read by the bare `meta` its decision makes — is neither pressed nor
/// escalated, and the loop parks. NEGATIVE CONTROL: the word unset, the same
/// box is pressed.
#[test]
fn an_autopilot_off_presses_no_box() {
    let (dir, journal) = journal_file("autopilot-box");
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let mut m = fenced(vec![bash_one_row(), idle_screen()]);
    m.autopilot = Some("-");
    m.autopilot_at = vec![(2, Some("off"))];
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(count(&m, "key"), 0, "nothing pressed: {:#?}", m.requests);
    assert_eq!(m.attention, None, "nothing escalated: {:#?}", m.requests);
    let said = journaled(&journal);
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=101")),
        "{said:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("APPROVED")),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let mut m = fenced(vec![bash_one_row(), idle_screen()]);
    m.autopilot = Some("-");
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &auto(30));
    assert!(
        lines.iter().any(|l| l.starts_with("APPROVED seq=")),
        "{lines:#?} {:#?}",
        m.requests
    );
}

/// OFF TOUCHES NO RATING CARD, not on the wait's busy reads either (the
/// review of a295792e4): under `dismiss_surveys` a card over a worker that
/// reads busy is dismissed at the first busy read that shows it — a key
/// pressed before the look reaches its park — so an Off loop is lent no such
/// duty: no `0`, no `DISMISSED` line, and the loop parks at the turn's
/// point. NEGATIVE CONTROL: the same screens under the word `cruise` get
/// the `0`.
#[test]
fn an_autopilot_off_dismisses_no_card_over_a_busy_worker() {
    let run = |word: &'static str| {
        let (dir, journal) = journal_file(&format!("autopilot-card-{word}"));
        let mut m = Mock::new(vec![
            with_survey(busy_screen()),
            busy_screen(),
            idle_screen(),
        ]);
        m.autopilot = Some(word);
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..dismissing()
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, said)
    };
    let (m, lines, said) = run("off");
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("DISMISSED")),
        "{lines:#?}"
    );
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=103")),
        "{said:#?}"
    );

    // NEGATIVE CONTROL: Cruise dismisses it on the busy read.
    let (m, lines, _) = run("cruise");
    assert_eq!(
        m.presses(),
        ["key if=^●.How.is.Claude.doing 0"],
        "{:#?}",
        m.requests
    );
    assert!(
        lines.contains(&"DISMISSED survey seq=101".to_string()),
        "{lines:#?}"
    );
}

/// The session starts in Cruise, then its owner changes the word as the
/// first busy screen is read. The watcher owns its own claim (a CLI watch
/// is not stopped by the window's host), so it must read that change before
/// pressing the rating card. The same card in Cruise really takes its key.
#[test]
fn a_busy_wait_reads_a_new_off_word_before_dismissing_a_card() {
    let run = |word| {
        let mut m = Mock::new(vec![
            with_survey(busy_screen()),
            busy_screen(),
            idle_screen(),
        ]);
        m.autopilot = Some("cruise");
        m.autopilot_on.insert(0, Some(word));
        m.vanish_after = Some(1);
        let (lines, _) = watch_lines(&mut m, &dismissing());
        assert_eq!(m.autopilot, Some(word), "the owner changed the word");
        (m, lines)
    };
    let (m, lines) = run("off");
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("DISMISSED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);

    let (m, lines) = run("cruise");
    assert_eq!(
        m.presses(),
        ["key if=^●.How.is.Claude.doing 0"],
        "{:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.starts_with("DISMISSED survey")),
        "{lines:#?}"
    );
}

/// A refusal to read the current word proves no permission for a busy-read
/// action. Retain the last known word for parking, but do not press the card
/// on that stale Cruise reading; the next successful read confirms Off.
#[test]
fn a_busy_wait_with_an_unanswered_autopilot_read_touches_no_card() {
    let mut m = Mock::new(vec![
        with_survey(busy_screen()),
        busy_screen(),
        idle_screen(),
    ]);
    m.autopilot = Some("cruise");
    let first = ok(&m.meta_line());
    m.verb_replies
        .insert("meta", VecDeque::from([first, err("denied: meta")]));
    m.autopilot_on.insert(0, Some("off"));
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert_eq!(m.autopilot, Some("off"), "the owner changed the word");
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("DISMISSED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

/// The requests `m` served, in arrival order, the claim's included.
fn order(m: &Mock) -> Vec<&str> {
    m.order.iter().map(|(r, _)| r.as_str()).collect()
}

/// OFF LOOKS UNSUPERVISED. As the loop parks it gives the session's claim
/// back — `meta unset supervisor holder=<it>`, the loop's own release — and
/// nothing renews or retries it while parked, though every request would (a
/// renewal step of zero), so the server raises the session's boxes to the
/// menu bar again; as the word turns, the claim is taken again before
/// anything is typed. NEGATIVE CONTROL: Cruise throughout gives nothing back.
#[test]
fn an_autopilot_off_park_gives_the_claim_back_and_takes_it_again() {
    let script = || {
        vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
        ]
    };
    let claimed = |s: &mut Watched<'_, '_>| {
        s.set_supervisor_name(Some("gui:7".to_string()));
        s.set_claim_timing(Duration::ZERO, 90_000);
    };
    let mut m = Mock::new(script());
    m.autopilot = Some("off");
    m.autopilot_at = vec![(3, Some("cruise"))];
    m.vanish_after = Some(4);
    let (lines, _) = watch_lines_with(&mut m, &under(owners_policy()), claimed);
    let order = order(&m);
    let given = order
        .iter()
        .position(|r| *r == "meta unset supervisor holder=gui:7")
        .unwrap_or_else(|| panic!("given back: {order:#?}"));
    // The bare `meta` that read `cruise` (the third: the reconcile's, then
    // the park's) ends the park.
    let metas: Vec<usize> = order
        .iter()
        .enumerate()
        .filter(|(_, r)| **r == "meta")
        .map(|(i, _)| i)
        .collect();
    let turned = metas[2];
    assert!(given < turned, "{order:#?}");
    assert!(
        metas[1] > given,
        "the park read its word after giving it back: {order:#?}"
    );
    assert!(
        !order[given + 1..turned]
            .iter()
            .any(|r| r.starts_with("meta set supervisor")),
        "nothing renewed or retried while parked: {order:#?}"
    );
    let retaken = order[turned..]
        .iter()
        .position(|r| *r == "meta set supervisor gui:7 ttl=90000")
        .map(|i| i + turned)
        .unwrap_or_else(|| panic!("taken again: {order:#?}"));
    let typed = order
        .iter()
        .position(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("continued: {lines:#?}"));
    assert!(retaken < typed, "{order:#?}");
    assert!(
        order[..given].contains(&"meta set supervisor gui:7 ttl=90000"),
        "held before the park: {order:#?}"
    );

    // NEGATIVE CONTROL: Cruise throughout — nothing given back (the session
    // ends gone, with no claim left to release).
    let mut m = Mock::new(script());
    m.autopilot = Some("cruise");
    m.vanish_after = Some(2);
    let _ = watch_lines_with(&mut m, &under(owners_policy()), claimed);
    assert!(
        !m.claims.iter().any(|c| c.contains("unset")),
        "{:#?}",
        m.claims
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
}

/// AN OFF PARK ENDS THE LAND EPISODE: Land types its instruction, the word
/// turns `off` at the next point (the loop parks there), and `land` again
/// — and the point, read afresh, gets the instruction again: a new episode,
/// never a nudge of the old one. NEGATIVE CONTROL: Land throughout nudges
/// at that point.
#[test]
fn land_then_off_then_land_types_the_land_instruction_again() {
    let script = || {
        vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
            busy_screen(),
            worked("Committing the parser now; the lexer is next."),
        ]
    };
    let run = |m: &mut Mock, tag: &str| {
        let (dir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..under(owners_policy())
        };
        let (lines, _) = watch_lines(m, &opts);
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (lines, said)
    };
    let typed = |lines: &[String], rule: &str| {
        lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED seq=") && l.contains(&format!(" rule={rule} ")))
            .count()
    };
    // The bare `meta` reads: the reconcile's, the instruction's decision,
    // the second point's decision (`off`: parked), the park's (`land`).
    let mut m = Mock::new(script());
    m.autopilot = Some("land");
    m.autopilot_at = vec![(3, Some("off")), (4, Some("land"))];
    m.turn_releases = Some(1);
    m.vanish_after = Some(4);
    let (lines, said) = run(&mut m, "autopilot-land-off-land");
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=104")),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|l| l.starts_with("UNPARKED seq=104 autopilot=land ")),
        "{said:#?}"
    );
    assert_eq!(typed(&lines, RULE_LAND), 2, "{lines:#?}");
    assert_eq!(typed(&lines, RULE_LAND_NUDGE), 0, "{lines:#?}");
    assert_eq!(count(&m, &continue_request(LAND)), 2, "{:#?}", m.requests);

    // NEGATIVE CONTROL: Land throughout — the second point is the old
    // episode's, and gets its nudge.
    let mut m = Mock::new(script());
    m.autopilot = Some("land");
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, said) = run(&mut m, "autopilot-land-throughout");
    assert!(!said.iter().any(|l| l.starts_with("PARKED")), "{said:#?}");
    assert_eq!(typed(&lines, RULE_LAND), 1, "{lines:#?}");
    assert_eq!(typed(&lines, RULE_LAND_NUDGE), 1, "{lines:#?}");
}

/// A REFUSED READ OF THE WORD KEEPS THE WORD LAST READ: a session the owner
/// turned `off` stays parked through a `meta` the server refuses — never
/// handed to the table's Cruise by the refusal — and nothing is typed.
/// NEGATIVE CONTROL: the same read answered `cruise` ends the park, and the
/// point is continued.
#[test]
fn a_refused_read_of_the_word_keeps_the_word_last_read() {
    let script = || {
        vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
        ]
    };
    // The reconcile's read says `off`; the park's first read is `second`;
    // every read after it, `after`.
    let run = |second: CtlReply, after: &'static str, tag: &str| {
        let mut m = Mock::new(script());
        m.autopilot = Some("off");
        let first = ok(&m.meta_line());
        m.verb_replies
            .insert("meta", VecDeque::from([first, second]));
        m.autopilot_at = vec![(1, Some(after))];
        m.vanish_after = Some(3);
        let (dir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..under(owners_policy())
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, said)
    };
    let (m, lines, said) = run(err("denied: meta"), "off", "autopilot-refused");
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=102")),
        "{said:#?}"
    );
    assert!(!said.iter().any(|l| l.starts_with("UNPARKED")), "{said:#?}");
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("EXIT session gone")),
        "{lines:#?}"
    );

    // NEGATIVE CONTROL: the same read answered, `cruise`.
    let cruise = {
        let mut m = Mock::new(script());
        m.autopilot = Some("cruise");
        ok(&m.meta_line())
    };
    let (m, _, said) = run(cruise, "cruise", "autopilot-refused-control");
    assert!(
        said.iter()
            .any(|l| l.starts_with("UNPARKED seq=102 autopilot=cruise ")),
        "{said:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
}

/// A MANAGER'S ONE LOOK (`supervise`) IS THE MANAGER'S: the session's word
/// `off` decides nothing there — no park, the read-only box pressed, the
/// write box handed over — and no claim is taken or given back. NEGATIVE
/// CONTROL: the harness's own loop (`watch`) over the same word parks and
/// presses nothing.
#[test]
fn a_managers_one_look_ignores_the_autopilot_word() {
    let (dir, journal) = journal_file("autopilot-one-look");
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let mut m = fenced(vec![bash_one_row(), busy_screen(), write_prompt()]);
    m.autopilot = Some("off");
    let (out, code) = session(&mut m, None).supervise(&opts).expect("supervise");
    assert_eq!(code, 0);
    assert!(
        out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
        "handed over: {out}"
    );
    assert_eq!(m.presses().len(), 1, "the read pressed: {:#?}", m.requests);
    assert!(m.claims.is_empty(), "{:#?}", m.claims);
    assert!(
        !journaled(&journal).iter().any(|l| l.starts_with("PARKED")),
        "{:#?}",
        journaled(&journal)
    );
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: the harness's own loop.
    let (dir, journal) = journal_file("autopilot-one-look-watch");
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let mut m = fenced(vec![bash_one_row(), idle_screen()]);
    m.autopilot = Some("off");
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        journaled(&journal).iter().any(|l| l.starts_with("PARKED")),
        "{:#?}",
        journaled(&journal)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE RETIRED TOKEN LANDS NOTHING: a reply to the land instruction that
/// ends on `ATERM-TASK-DONE` beside work left is no clean defense, so Land
/// nudges it (`land-nudge@v1`) and journals no `DONE`. Under Cruise the same
/// reply, and a token under a report that leaves nothing, are done claims
/// that get the done check (FOREVER CRUISE), never an end.
#[test]
fn a_token_reply_to_the_land_instruction_is_nudged_never_landed() {
    let done_lines = |word: &'static str, tag: &str, reply: &str| {
        let mut m = Mock::new(vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
            busy_screen(),
            worked(reply),
        ]);
        m.autopilot = Some(word);
        m.turn_releases = Some(1);
        m.vanish_after = Some(4);
        // A wait on the screen that sits passes the time it asks: an act
        // the claim's back-off holds comes inside the run.
        m.stall_sleep = Some(Duration::from_millis(20));
        let (dir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..under(owners_policy())
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        let all = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        all
    };
    let left = "Committed the parser. Left: the lexer.\nATERM-TASK-DONE";
    let clean = "Committed the parser and the lexer. Nothing left.\nATERM-TASK-DONE";
    for (word, tag, reply, rule) in [
        ("land", "autopilot-land-token", left, RULE_LAND_NUDGE),
        ("cruise", "autopilot-cruise-token", left, RULE_DONE_CHECK),
        ("cruise", "autopilot-cruise-clean", clean, RULE_DONE_CHECK),
    ] {
        let all = done_lines(word, tag, reply);
        let said: Vec<&String> = all
            .iter()
            .filter(|l| {
                l.starts_with("DONE ")
                    || (l.starts_with("CONTINUED ") && !l.starts_with("CONTINUED seq=102 "))
            })
            .collect();
        assert_eq!(said.len(), 1, "{tag}: {all:#?}");
        assert!(
            said[0].starts_with("CONTINUED ") && said[0].contains(&format!(" rule={rule} ")),
            "{tag}: {all:#?}"
        );
    }
}

/// What a step of the loop SAID and what it only NOTED (the journal alone).
#[derive(Default)]
struct Notes {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Notes {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.said.push(line.to_string());
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.noted.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        true
    }
}

/// The loop's own badge for the box `rows` shows, as its escalation writes
/// it.
fn badge_of(rows: &[String]) -> String {
    let turn = Turn {
        phase: worker_phase(rows),
        screen: Screen {
            rows: rows.to_vec(),
            ..Screen::default()
        },
        timed_out: false,
    };
    super::super::escalate::attention_text(&aterm_phase::ClaudeReader, &turn, "a restart")
}

/// OFF TAKES THE LOOP'S BADGES DOWN AS IT PARKS (the review of c1d193998):
/// the menu bar raises the session's boxes itself while the loop is parked,
/// so a badge of the loop's left standing would tell the person twice, and
/// the harness's stats would count the whole park as a person's wait. A
/// box's badge the loop adopted at its start is unset as the park starts —
/// journaled `CLEARED seq=<n> box attention=OK`, before the claim is given
/// back — and once the word turns `cruise`, the box still showing is
/// escalated again (the post-park rejudge). NEGATIVE CONTROL: Cruise
/// throughout keeps the adopted badge: nothing unset, no second badge.
#[test]
fn an_off_park_takes_the_loops_badges_down_and_the_rejudge_raises_them_again() {
    let rm = write_prompt();
    let ours = badge_of(&rm);
    let claimed = |s: &mut Watched<'_, '_>| {
        s.set_supervisor_name(Some("gui:7".to_string()));
    };
    let (dir, journal) = journal_file("autopilot-off-badges");
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let mut m = Mock::new(vec![rm.clone(), rm.clone(), rm.clone()]);
    m.attention = Some(ours.clone());
    m.autopilot = Some("off");
    m.autopilot_at = vec![(3, Some("cruise"))];
    // The run ends at the first wait after the badge is raised again.
    m.exit_when = Some(|m| m.attentions >= 1);
    let _ = watch_lines_with(&mut m, &opts, claimed);
    let said = journaled(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let at = |head: &str| {
        said.iter()
            .position(|l| l.starts_with(head))
            .unwrap_or_else(|| panic!("{head}: {said:#?}"))
    };
    let (adopted, parked, cleared, unparked, raised) = (
        at("ADOPTED seq="),
        at("PARKED seq="),
        at("CLEARED seq="),
        at("UNPARKED seq="),
        at("ESCALATED seq="),
    );
    assert!(
        adopted < parked && parked < cleared && cleared < unparked && unparked < raised,
        "{said:#?}"
    );
    assert!(
        said[cleared].ends_with(" box attention=OK"),
        "{}",
        said[cleared]
    );
    let order = order(&m);
    let unset = order
        .iter()
        .position(|r| *r == "meta unset attention owner=supervisor")
        .unwrap_or_else(|| panic!("the badge unset: {order:#?}"));
    let given = order
        .iter()
        .position(|r| *r == "meta unset supervisor holder=gui:7")
        .unwrap_or_else(|| panic!("the claim given back: {order:#?}"));
    assert!(unset < given, "the badge before the claim: {order:#?}");
    assert_eq!(m.attentions, 1, "raised again: {:#?}", m.requests);
    // The same box's badge, its reason the policy's own this time.
    let label = ours.split(" (").next().unwrap_or_default();
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.starts_with(&format!("{label} ("))),
        "{:?}",
        m.attention
    );

    // NEGATIVE CONTROL: Cruise throughout keeps the adopted badge.
    let (dir, journal) = journal_file("autopilot-cruise-badges");
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let mut m = Mock::new(vec![rm.clone(), rm.clone(), rm]);
    m.attention = Some(ours.clone());
    m.autopilot = Some("cruise");
    m.vanish_after = Some(3);
    let _ = watch_lines_with(&mut m, &opts, claimed);
    let said = journaled(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        said.iter().any(|l| l.starts_with("ADOPTED seq=")),
        "{said:#?}"
    );
    assert!(!said.iter().any(|l| l.starts_with("CLEARED")), "{said:#?}");
    assert_eq!(count(&m, "meta unset attention"), 0, "{:#?}", m.requests);
    assert_eq!(m.attentions, 0, "no second badge: {:#?}", m.requests);
    assert_eq!(m.attention.as_deref(), Some(ours.as_str()));
}

/// EACH BADGE AN OFF PARK TAKES DOWN IS JOURNALED (`Session::lower_badges`,
/// the claim untouched): the turn-end policy's (`CLEARED seq=<n> turn-end
/// attention=OK`), a limit episode's and a limit's adopted before its
/// episode opened (`CLEARED seq=<n> attention=OK <why>`). (What each leaves
/// for the park's end — the turn-end text, the limit's episode — is
/// `an_off_parks_end_sets_the_turn_end_badge_again_unless_the_worker_worked`'s
/// and `a_limit_spanning_an_off_park_is_one_episode_one_escalation`'s.)
/// NEGATIVE CONTROL: a loop that holds no badge sends and
/// says nothing.
#[test]
fn an_off_parks_badges_come_down_each_with_its_cleared_line() {
    let lowered = |setup: fn(&mut Session<'_, Mock>), entry: &str| {
        let mut m = Mock::new(vec![idle_screen()]);
        m.attention = Some(entry.to_string());
        let mut notes = Notes::default();
        {
            let mut s = session(&mut m, None);
            s.attention_ours = Some(entry.to_string());
            setup(&mut s);
            assert_eq!(s.lower_badges(101, &mut notes), Ok(()));
            assert!(!s.turn_end_badge && s.limit.is_none());
        }
        (notes, m)
    };
    let why = "the session's autopilot is off";

    // The turn-end policy's badge (`/login` typed, say).
    let (notes, m) = lowered(
        |s| s.turn_end_badge = true,
        "claude idle: finish sign-in in the browser (the turn-end policy)",
    );
    assert_eq!(notes.noted, ["CLEARED seq=101 turn-end attention=OK"]);
    assert_eq!(m.attention, None);
    assert!(notes.said.is_empty(), "journal-only: {:#?}", notes.said);

    // A limit episode's badge.
    let (notes, m) = lowered(
        |s| {
            s.limit = Some(Episode {
                seq: 90,
                resumes_itself: false,
                reset_unix: None,
                reset_text: "-".to_string(),
                extended: false,
                raised: true,
            });
        },
        "limited: You've hit your session limit",
    );
    assert_eq!(notes.noted, [format!("CLEARED seq=101 attention=OK {why}")]);
    assert_eq!(m.attention, None);

    // A limit's badge adopted at the start, its episode not open yet.
    let (notes, m) = lowered(
        |s| s.adopted_limit = true,
        "limited: You've hit your session limit",
    );
    assert_eq!(notes.noted, [format!("CLEARED seq=101 attention=OK {why}")]);
    assert_eq!(m.attention, None);

    // NEGATIVE CONTROL: no badge held.
    let (notes, m) = lowered(|_| {}, "claude idle: a person's question (x)");
    assert!(
        notes.noted.is_empty() && notes.said.is_empty(),
        "{:#?}",
        notes.noted
    );
    assert!(m.requests.is_empty(), "{:#?}", m.requests);
}

/// THE LOOP'S LAST CLEAR OF THE TURN-END POLICY'S BADGE IS JOURNALED
/// (`Session::release_attention`, the review of c1d193998): `CLEARED
/// seq=<n> turn-end attention=OK`, so the harness's stats close the
/// person's wait on it rather than dropping it at the loop's end — the line
/// the fold reads back (`harness::stats`, a wait from its escalation to this
/// line). NEGATIVE CONTROL: a loop whose turn-end badge is down says and
/// unsets nothing for it, and without the line the fold counts no wait.
#[test]
fn the_loops_last_clear_of_the_turn_end_badge_is_journaled() {
    use crate::harness::stats::{HarnessStats, fold_journal};
    use crate::supervise::journal::JournalRecord;
    let entry = "claude idle: finish sign-in in the browser (the turn-end policy)";
    let released = |badge: bool| {
        let mut m = Mock::new(vec![idle_screen()]);
        m.attention = Some(entry.to_string());
        let mut notes = Notes::default();
        {
            let mut s = session(&mut m, None);
            s.attention_ours = Some(entry.to_string());
            s.turn_end_badge = badge;
            s.release_attention(&mut notes);
        }
        (notes, m)
    };
    let (notes, m) = released(true);
    assert_eq!(notes.noted, ["CLEARED seq=0 turn-end attention=OK"]);
    assert_eq!(m.attention, None);
    // The stats fold, over the escalation and this end: a 4 s wait.
    let wait = |noted: &[String]| {
        let mut recs = vec![JournalRecord::of_line(
            1_000,
            Some("s-1"),
            "ESCALATED seq=7 attention=OK",
            None,
        )];
        recs.extend(
            noted
                .iter()
                .map(|l| JournalRecord::of_line(5_000, Some("s-1"), l, None)),
        );
        recs.push(JournalRecord::of_line(
            5_000,
            Some("s-1"),
            "EXIT stopped",
            None,
        ));
        let mut acc = HarnessStats::new(0);
        fold_journal(&mut acc, &recs);
        acc.human_wait_ms
    };
    assert_eq!(wait(&notes.noted), 4_000);

    // NEGATIVE CONTROL: no turn-end badge.
    let (notes, m) = released(false);
    assert!(notes.noted.is_empty(), "{:#?}", notes.noted);
    assert_eq!(m.attention.as_deref(), Some(entry));
    assert_eq!(wait(&notes.noted), 0, "the loop's end counts nothing");
}

/// A PARK'S RETAKE BEHIND THE SAME HOLDER SAYS NOTHING NEW (the review of
/// c1d193998): the holder the loop watched behind as it parked is kept with
/// the claim given back, so `WATCHING another supervisor holds this
/// session: <holder>` is said once a holder, not once a park; and a claim
/// won at the retake is news (`CLAIMED … (after <holder>)`). NEGATIVE
/// CONTROL: a retake that finds ANOTHER holder says so.
#[test]
fn a_parks_retake_behind_the_same_holder_says_it_once() {
    let busy = |holder: &str| err(&format!("busy supervisor={holder}"));
    let mut m = Mock::new(vec![idle_screen()]);
    m.claim_replies = [
        busy("gui%3A42"),
        busy("gui%3A42"),
        busy("gui%3A9"),
        ok("OK\n"),
    ]
    .into();
    let mut notes = Notes::default();
    {
        let mut s = session(&mut m, None);
        s.set_supervisor_name(Some("gui:7".to_string()));
        s.take_claim(&mut notes);
        assert_eq!(s.claim.watching_behind(), Some("gui:42"));
        s.give_back_claim();
        assert_eq!(
            s.claim,
            claim::Claim::GivenBack {
                behind: Some("gui:42".to_string())
            }
        );
        // The same holder at the retake: nothing said.
        s.retake_claim(&mut notes);
        assert_eq!(s.claim.watching_behind(), Some("gui:42"));
        assert_eq!(
            notes.said,
            ["WATCHING another supervisor holds this session: gui:42"]
        );
        // NEGATIVE CONTROL: another holder at the next retake is said.
        s.give_back_claim();
        s.retake_claim(&mut notes);
        assert_eq!(
            notes.said[1..],
            ["WATCHING another supervisor holds this session: gui:9"]
        );
        // A claim won at a retake from behind is news.
        s.give_back_claim();
        s.retake_claim(&mut notes);
        assert_eq!(
            notes.noted,
            ["CLAIMED supervisor=gui:7 (after gui:9)"],
            "{:#?}",
            notes.said
        );
    }
    assert!(
        !m.claims.iter().any(|c| c.contains("unset")),
        "behind, nothing of the loop's to release: {:#?}",
        m.claims
    );
}

/// AN OFF PARK'S END SETS THE TURN-END POLICY'S BADGE AGAIN (the review of
/// b512d03d8). That badge carries no point the rejudge raises it again from
/// — `/login`'s "finish sign-in", a switch's note, a Codex sandbox fall,
/// each said once — so the park keeps its text as it takes it down and,
/// with the word turned, sets it again: one `meta set attention`, journaled
/// `ESCALATED … again=set again after an off park` after the `UNPARKED`
/// line, and the badge the
/// loop's again, cleared as before once the worker works. NEGATIVE CONTROL:
/// a read of the park that sees the worker working is that badge's end —
/// nothing set again.
#[test]
fn an_off_parks_end_sets_the_turn_end_badge_again_unless_the_worker_worked() {
    let entry = "claude idle: finish sign-in in the browser (the turn-end policy)";
    // A park on the point at seq 100 over `screens`, its word turning
    // `cruise` at its second read.
    let parked = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        m.attention = Some(entry.to_string());
        m.autopilot = Some("off");
        m.autopilot_at = vec![(2, Some("cruise"))];
        let meta = m.meta_line();
        let mut notes = Notes::default();
        let badge = {
            let mut s = session(&mut m, None);
            s.note_meta(&meta);
            s.attention_ours = Some(entry.to_string());
            s.turn_end_badge = true;
            let deadline = Instant::now() + Duration::from_secs(60);
            let opts = under(owners_policy());
            assert_eq!(s.park_while_off(100, deadline, &opts, &mut notes), Ok(()));
            s.turn_end_badge
        };
        (notes, m, badge)
    };
    let (notes, m, badge) = parked(vec![idle_screen()]);
    let at = |head: &str| {
        notes
            .noted
            .iter()
            .position(|l| l.starts_with(head))
            .unwrap_or_else(|| panic!("{head}: {:#?}", notes.noted))
    };
    let (cleared, unparked, raised) = (
        at("CLEARED seq=100 turn-end attention=OK"),
        at("UNPARKED seq="),
        at("ESCALATED seq="),
    );
    assert!(
        cleared < unparked && unparked < raised,
        "{:#?}",
        notes.noted
    );
    assert!(
        notes.noted[raised].ends_with(&format!(" attention=OK again={AGAIN_UNPARKED}")),
        "{}",
        notes.noted[raised]
    );
    assert_eq!(m.attention.as_deref(), Some(entry));
    assert_eq!(m.attentions, 1, "set once, again: {:#?}", m.requests);
    assert!(
        badge,
        "the loop's badge again, cleared once the worker works"
    );

    // NEGATIVE CONTROL: the worker read working while parked.
    let (notes, m, badge) = parked(vec![busy_screen(), idle_screen()]);
    assert!(
        notes.noted.iter().any(|l| l.starts_with("UNPARKED seq=")),
        "{:#?}",
        notes.noted
    );
    assert!(
        !notes.noted.iter().any(|l| l.starts_with("ESCALATED")),
        "{:#?}",
        notes.noted
    );
    assert_eq!(m.attention, None);
    assert_eq!(m.attentions, 0, "{:#?}", m.requests);
    assert!(!badge);
}

/// A LIMIT'S BADGE SAYS WHETHER IT IS WAITED OUT
/// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2.4): under `resume_limits`,
/// a notice naming a reset the loop read is `kind=wait` — the turn-end policy
/// continues it — and a notice naming no reset it can read is `kind=ask`: a
/// reset is never guessed, so nothing here ends it. One Claude Code continues
/// by itself is `kind=wait` with the limits resumed or not: nobody acts on
/// it. NEGATIVE CONTROL: the reset-naming notice with the limits not resumed
/// is `kind=ask`. The text is the notice's either way, and the episode's own
/// reading of it is unchanged.
#[test]
fn a_limit_badge_is_a_wait_only_where_the_loop_waits_it_out() {
    let weekly = "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
    let auto_continue =
        "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel";
    // A spend limit no reset ends: the vendor names none.
    let spend = "You're out of extra usage · add funds to continue";
    let reset = Some("Sep 19 at 11am (America/Los_Angeles)");
    for (resume_limits, notice, reset, kind) in [
        (true, weekly, reset, "kind=wait"),
        (true, auto_continue, None, "kind=wait"),
        (true, spend, None, "kind=ask"),
        (false, weekly, reset, "kind=ask"),
        // Claude Code continues by itself whatever the owner's policy says
        // of the loop's own continuation.
        (false, auto_continue, None, "kind=wait"),
    ] {
        let mut m = Mock::new(vec![limited_screen()]);
        let mut notes = Notes::default();
        {
            let mut s = session(&mut m, None);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.resume_limits = resume_limits;
            assert_eq!(
                s.open_episode(102, notice, reset, false, &mut notes),
                Ok(())
            );
            assert_eq!(
                s.limit.as_ref().map(|ep| ep.resumes_itself),
                Some(notice == auto_continue)
            );
        }
        let entry = format!("{ATTENTION_PREFIX} {notice}");
        assert_eq!(
            m.attention_kind.as_deref(),
            Some(kind),
            "{notice}: {:?}",
            m.requests
        );
        assert_eq!(m.attention.as_deref(), Some(entry.as_str()), "{notice}");
        assert_eq!(m.attentions, 1, "{notice}: {:?}", m.requests);
    }
}

/// A LIMIT SPANNING AN OFF PARK IS ONE EPISODE (the review of b512d03d8).
/// The park takes the episode's badge down and keeps the episode as a
/// retry, so the notice still up after the park opens the SAME episode —
/// its seq kept, its badge set again with no second ask (`again=the retry
/// hit the wall again, the episode of seq=90`) — and the harness's stats
/// fold one escalation, its wait leaving the park out. A limit's badge
/// adopted from a previous loop keeps its marker across the park the same
/// way (`again=adopted …`). NEGATIVE CONTROL: an episode the worker's answer
/// closed keeps no retry, and the notice met again is a new episode, a new
/// ask — two escalations.
#[test]
fn a_limit_spanning_an_off_park_is_one_episode_one_escalation() {
    use crate::harness::stats::{HarnessStats, fold_journal};
    use crate::supervise::journal::JournalRecord;
    let notice = "You've hit your session limit · resets 2am";
    let entry = format!("{ATTENTION_PREFIX} {notice}");
    let episode = |s: &mut Session<'_, Mock>| {
        s.limit = Some(Episode {
            seq: 90,
            resumes_itself: false,
            reset_unix: None,
            reset_text: "-".to_string(),
            extended: false,
            raised: true,
        });
    };
    // The badge `setup` leaves standing, taken down by a park
    // (`lower_badges`) or closed by the worker's answer, then the notice met
    // again at seq 102: the lines noted, the requests, the episode's seq.
    let run = |setup: fn(&mut Session<'_, Mock>), park: bool| {
        let mut m = Mock::new(vec![limited_screen()]);
        m.attention = Some(entry.clone());
        let mut notes = Notes::default();
        let seq = {
            let mut s = session(&mut m, None);
            s.attention_ours = Some(entry.clone());
            setup(&mut s);
            if park {
                assert_eq!(s.lower_badges(100, &mut notes), Ok(()));
            } else {
                assert_eq!(
                    s.close_episode(100, "answered after a busy spell", &mut notes),
                    Ok(())
                );
            }
            assert_eq!(s.open_episode(102, notice, None, false, &mut notes), Ok(()));
            s.limit.as_ref().map(|ep| ep.seq)
        };
        (notes.noted, m, seq)
    };
    // The episode's first escalation at 1 s, then the lines noted at 2 s
    // (the close) and 5 s (the notice again), and its end at 6 s.
    let folded = |noted: &[String]| {
        let mut recs = vec![JournalRecord::of_line(
            1_000,
            Some("s-1"),
            "ESCALATED seq=90 attention=OK",
            None,
        )];
        recs.extend(
            noted
                .iter()
                .zip([2_000, 5_000])
                .map(|(l, t)| JournalRecord::of_line(t, Some("s-1"), l, None)),
        );
        recs.push(JournalRecord::of_line(
            6_000,
            Some("s-1"),
            "CLEARED seq=103 attention=OK answered after a busy spell",
            None,
        ));
        let mut acc = HarnessStats::new(0);
        fold_journal(&mut acc, &recs);
        (acc.escalated, acc.human_wait_ms)
    };

    let (noted, m, seq) = run(episode, true);
    assert_eq!(
        noted,
        [
            "CLEARED seq=100 attention=OK the session's autopilot is off".to_string(),
            format!("ESCALATED seq=102 attention=OK again={AGAIN_RETRY}, the episode of seq=90"),
        ]
    );
    assert_eq!(seq, Some(90), "the same episode");
    assert_eq!(m.attentions, 1, "set again once: {:#?}", m.requests);
    assert_eq!(
        folded(&noted),
        (1, 2_000),
        "one escalation, the park left out"
    );

    // A badge adopted from a previous loop, its episode not open yet.
    let (noted, _, _) = run(|s| s.adopted_limit = true, true);
    assert_eq!(
        noted,
        [
            "CLEARED seq=100 attention=OK the session's autopilot is off".to_string(),
            format!("ESCALATED seq=102 attention=OK again={AGAIN_ADOPTED}"),
        ]
    );

    // NEGATIVE CONTROL: closed by the worker's answer — a new episode.
    let (noted, _, seq) = run(episode, false);
    assert_eq!(seq, Some(102), "{noted:#?}");
    assert!(
        noted.iter().any(|l| l == "ESCALATED seq=102 attention=OK"),
        "{noted:#?}"
    );
    assert_eq!(folded(&noted).0, 2, "{noted:#?}");
}

/// WHAT A PARK KEEPS OF A LIMIT ENDS WHERE THE WORKER ANSWERED PAST IT (the
/// review of a295792e4): the retry a park keeps of a raised episode, and the
/// marker of a limit's badge adopted from a previous loop, last only while
/// the notice stands. A read of the park that shows the worker answered —
/// a point that is not the notice — ends both, so the limit met after the
/// word turns is a NEW episode: its own seq, a new ask, an escalation the
/// stats count. NEGATIVE CONTROL: the notice still up at the park's read
/// keeps the episode, its badge set again with no second ask.
#[test]
fn a_limit_the_worker_answered_past_in_a_park_is_a_new_episode() {
    let notice = "You've hit your session limit · resets 2am";
    let entry = format!("{ATTENTION_PREFIX} {notice}");
    let episode = |s: &mut Session<'_, Mock>| {
        s.limit = Some(Episode {
            seq: 90,
            resumes_itself: false,
            reset_unix: None,
            reset_text: "-".to_string(),
            extended: false,
            raised: true,
        });
    };
    // The badge `setup` leaves standing, a park whose read shows `read` and
    // whose word turns `cruise` at its second read, then the notice met at
    // seq 102: what was noted, the requests, the episode's seq.
    let run = |setup: fn(&mut Session<'_, Mock>), read: Vec<String>| {
        let mut m = Mock::new(vec![read]);
        m.attention = Some(entry.clone());
        m.autopilot = Some("off");
        m.autopilot_at = vec![(2, Some("cruise"))];
        let meta = m.meta_line();
        let mut notes = Notes::default();
        let seq = {
            let mut s = session(&mut m, None);
            s.note_meta(&meta);
            s.attention_ours = Some(entry.clone());
            setup(&mut s);
            let deadline = Instant::now() + Duration::from_secs(60);
            let opts = under(owners_policy());
            assert_eq!(s.park_while_off(100, deadline, &opts, &mut notes), Ok(()));
            assert_eq!(s.open_episode(102, notice, None, false, &mut notes), Ok(()));
            s.limit.as_ref().map(|ep| ep.seq)
        };
        (notes.noted, m, seq)
    };
    let escalated = |noted: &[String]| -> String {
        noted
            .iter()
            .find(|l| l.starts_with("ESCALATED seq=102 "))
            .cloned()
            .unwrap_or_else(|| panic!("{noted:#?}"))
    };
    for (what, setup) in [
        ("a raised episode", episode as fn(&mut Session<'_, Mock>)),
        ("an adopted badge", |s| s.adopted_limit = true),
    ] {
        // The worker answered while parked: a new episode, a new ask.
        let (noted, _, seq) = run(setup, idle_screen());
        assert_eq!(seq, Some(102), "{what}: {noted:#?}");
        assert!(!escalated(&noted).contains(" again="), "{what}: {noted:#?}");

        // NEGATIVE CONTROL: the notice still up — the same episode.
        let (noted, _, _) = run(setup, limited_screen());
        assert!(escalated(&noted).contains(" again="), "{what}: {noted:#?}");
    }
}

/// A LIMIT'S BADGE ADOPTED FROM A PREVIOUS LOOP IS OVER AT THE FIRST POINT
/// THAT IS NOT ITS NOTICE (the review of a295792e4): a loop that adopts the
/// badge over the notice, then reads the worker answered past it, takes the
/// badge down (`CLEARED … the worker answered past the adopted limit's
/// notice`) and forgets the adoption, so the next limit is escalated as its
/// own ask. NEGATIVE CONTROL: the notice still up at the first look is the
/// adopted episode — its badge set again, no new ask.
#[test]
fn a_limit_after_the_worker_answered_an_adopted_one_is_a_new_ask() {
    let notice = "You've hit your session limit · resets 7:30pm (America/Los_Angeles)";
    let run = |script: Vec<Vec<String>>, tag: &str| {
        let (dir, journal) = journal_file(tag);
        let mut m = Mock::new(script);
        m.attention = Some(format!("{ATTENTION_PREFIX} {notice}"));
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            // Nothing typed at the idle point, and a limit the policy does
            // not wait out itself: the person is told of it.
            ..under(SupervisorConfig {
                continue_policy: false,
                resume_limits: false,
                ..owners_policy()
            })
        };
        let _ = watch_lines_with(&mut m, &opts, |_| {});
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (m, said)
    };
    let (m, said) = run(
        vec![
            limited_screen(),
            busy_screen(),
            idle_screen(),
            busy_screen(),
            limited_screen(),
        ],
        "adopted-limit-answered",
    );
    let why = format!("{said:#?}\n{:#?}", m.requests);
    let at = |head: &str| {
        said.iter()
            .position(|l| l.starts_with(head))
            .unwrap_or_else(|| panic!("{head}: {why}"))
    };
    let (adopted, cleared, escalated) =
        (at("ADOPTED seq="), at("CLEARED seq="), at("ESCALATED seq="));
    assert!(adopted < cleared && cleared < escalated, "{why}");
    assert!(
        said[cleared].ends_with(" the worker answered past the adopted limit's notice"),
        "{why}"
    );
    assert!(!said[escalated].contains(" again="), "{why}");

    // NEGATIVE CONTROL: the notice still up at the first look.
    let (m, said) = run(vec![limited_screen()], "adopted-limit-standing");
    let why = format!("{said:#?}\n{:#?}", m.requests);
    assert!(
        said.iter()
            .any(|l| l.starts_with("ESCALATED seq=")
                && l.ends_with(&format!("again={AGAIN_ADOPTED}"))),
        "{why}"
    );
}

/// A LOOP BEHIND ANOTHER SUPERVISOR'S CLAIM READS THE SESSION'S WORD AT ITS
/// START (the review of b512d03d8): its start-up reconcile settles nothing
/// behind the holder, but the word its `meta` read names is noted — so a
/// park on the table's `off` knows the host keeps the word, reads it again
/// at each step, and ends when the owner turns this session `cruise`.
/// NEGATIVE CONTROL: behind the same holder on a host that keeps no word
/// (an older build), nothing is asked again and the park lasts the run.
#[test]
fn a_loop_behind_another_holder_reads_the_word_and_its_park_ends_on_it() {
    let run = |word: Option<&'static str>, tag: &str| {
        let mut m = Mock::new(vec![
            busy_screen(),
            worked("Fixed the parser; the suite is green."),
        ]);
        m.autopilot = word;
        m.autopilot_at = vec![(3, word.map(|_| "cruise"))];
        m.vanish_after = Some(4);
        m.claim_replies = [err("busy supervisor=gui%3A42")].into();
        let (dir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..under(SupervisorConfig {
                autopilot: Autopilot::Off,
                ..owners_policy()
            })
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_supervisor_name(Some("gui:7".to_string()));
        });
        let said = journaled(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, said)
    };
    let (m, lines, said) = run(Some("off"), "autopilot-behind-word");
    assert!(
        lines
            .iter()
            .any(|l| l == "WATCHING another supervisor holds this session: gui:42"),
        "{lines:#?}"
    );
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=")),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|l| l.starts_with("UNPARKED seq=") && l.contains(" autopilot=cruise ")),
        "{said:#?}"
    );
    assert!(
        m.metas >= 3,
        "the word read at each step: {:#?}",
        m.requests
    );

    // NEGATIVE CONTROL: a host that keeps no word.
    let (m, _, said) = run(None, "autopilot-behind-no-word");
    assert!(
        said.iter().any(|l| l.starts_with("PARKED seq=")),
        "{said:#?}"
    );
    assert!(!said.iter().any(|l| l.starts_with("UNPARKED")), "{said:#?}");
    assert_eq!(m.metas, 1, "the reconcile's read alone: {:#?}", m.requests);
}
