// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// A key request with its generation's number masked: the fence is asserted
fn ungen(request: &str) -> String {
    request
        .split(' ')
        .map(|word| match word.strip_prefix("if-gen=") {
            Some(_) => "if-gen=*",
            None => word,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The row guard of `row`, as a key request spells it.
fn guard_of(row: &str) -> String {
    crate::supervise::policy::row_guard(row)
}

/// A day and hour of the vendor's on the tests' clock and zone.
fn placed(when: &str) -> i64 {
    let spec = limit::parse_reset(when).expect("a dated clock time");
    limit::reset_at_with(&spec, TEST_NOW, PDT, &test_zones)
}

/// The whole of the confirmation's body and detail as the composed fixture
const RESET_HEAD: &str = "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am \
                          2 weekly resets left · use by Oct 5 at 3pm";

/// A reset spent, as a host reads it: the vendor queued its own
fn spent() -> ResetOutcome {
    ResetOutcome::Spent {
        kind: aterm_phase::ResetKind::Weekly,
        asks: false,
        left: Some(1),
        cleared_at: TEST_NOW,
    }
}

/// The whole path: with the key on and the host's go, the loop moves the
#[test]
fn the_whole_path_is_three_keys_and_the_hosts_no_is_todays_wait_row() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "whole",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    let presses: Vec<String> = m.presses().iter().map(|r| ungen(r)).collect();
    assert_eq!(
        presses,
        [
            format!(
                "key if-human=0 if-gen=* if={} down",
                guard_of("   What do you want to do?")
            ),
            format!(
                "key if-human=0 if-gen=* if={} enter",
                guard_of(&format!("   ❯ 2. {RESET_GRANT}"))
            ),
            format!(
                "key if-human=0 if-gen=* if={} enter",
                guard_of("   ❯ Yes, use my weekly reset")
            ),
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
    assert_eq!(
        (count(&m, "send"), count(&m, "turn")),
        (0, 0),
        "{:#?}",
        m.requests
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(reset_world(&mut m).chosen, [RESET_GRANT]);
    let ask = format!(
        "take kind=weekly left=2 day=Oct 5 day_start=Some({})",
        placed("Oct 5 at 12am")
    );
    assert_eq!(host.said("take"), [ask.clone(), ask]);
    assert_eq!(
        host.said("told"),
        [format!(
            "told entered=true kept=- offer=weekly 2 Some({}) last=false",
            placed("Oct 5 at 3pm")
        )]
    );
    assert!(host.said("inform").is_empty());
    assert!(
        run.lines
            .iter()
            .any(|l| l.starts_with("APPROVED seq=") && l.ends_with(RESET_GRANT)),
        "{:#?}",
        run.lines
    );
    assert!(
        run.lines.iter().any(|l| l.starts_with("RESET seq=")
            && l.ends_with("rule=limit-reset@v1 used (1 weekly reset left)")),
        "{:#?}",
        run.lines
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=")
                && l.ends_with(&format!("confirmed ({RESET_HEAD})"))),
        "{:#?}",
        run.journal
    );
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until {}", host.until),
            format!("approved|Yes, use my weekly reset|{RESET_HEAD}"),
            "waiting|limit reset|ended: used".to_string(),
        ]
    );
    assert_eq!(run.typed(), Vec::<&str>::new());
    assert_eq!(m.attention, None, "nothing asked of the owner");
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let through_last_key = |m: &Mock| {
        let last = m
            .requests
            .iter()
            .rposition(|r| verb_of(r) == "key")
            .expect("a key");
        m.requests[..=last].to_vec()
    };
    let off_host = ResetHost::reading(ResetOutcome::Unread);
    let mut off = reset_mock(vec![], &RESET_ROWS, 0, &off_host);
    let off_run = reset_watch(
        "off",
        &mut off,
        reset_opts(&off_host, false, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&off),
        ["down@dialog", "down@dialog", "enter@dialog"]
    );
    assert_eq!(reset_world(&mut off).chosen, [RESET_WAIT]);
    let off_presses: Vec<String> = off.presses().iter().map(|r| ungen(r)).collect();
    let arrow = format!(
        "key if-gen=* if={} down",
        guard_of("   What do you want to do?")
    );
    assert_eq!(
        off_presses,
        [
            arrow.clone(),
            arrow,
            format!(
                "key if-gen=* if={} enter",
                guard_of(&format!("   ❯ 3. {RESET_WAIT}"))
            ),
        ],
        "{:#?}",
        off.requests
    );
    assert!(off_host.said("").is_empty(), "the host is never asked");
    assert!(off_run.of_rule().is_empty(), "{:#?}", off_run.journal);
    assert!(off_run.rows().is_empty());
    assert!(
        off_run
            .ledger
            .iter()
            .any(|r| r.contains("limit-wait@v1") && r.contains("approved"))
    );
    let no_host = ResetHost::refusing(ResetNo::NotQualifying);
    let mut no = reset_mock(vec![], &RESET_ROWS, 0, &no_host);
    let no_run = reset_watch(
        "no",
        &mut no,
        reset_opts(&no_host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(through_last_key(&no), through_last_key(&off));
    assert_eq!(reset_world(&mut no).chosen, [RESET_WAIT]);
    assert_eq!(no_host.said("take").len(), 1, "asked once a mount");
    assert!(no_host.said("told").is_empty(), "no claim was taken");
    let said: Vec<&str> = no_run
        .of_rule()
        .into_iter()
        .filter(|l| l.starts_with("SKIPPED"))
        .collect();
    assert_eq!(said.len(), 1, "{:#?}", no_run.journal);
    assert!(said[0].ends_with("rule=limit-reset@v1 host=not-qualifying"));
    assert!(
        no_run.rows().is_empty(),
        "the pass writes no row of the rule"
    );
    for (m, host) in [(&off, &off_host), (&no, &no_host)] {
        assert_eq!(reset_violations(m, host), Vec::<String>::new());
    }
    let again_host = ResetHost::refusing(ResetNo::NotQualifying);
    let first = ResetWorld::new(&RESET_ROWS, 2, &again_host.shared).dialog();
    let mut again = reset_mock(vec![first, idle_screen()], &RESET_ROWS, 2, &again_host);
    let _ = reset_watch(
        "no-again",
        &mut again,
        reset_opts(&again_host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(reset_world(&mut again).chosen, [RESET_WAIT]);
    assert_eq!(
        again_host.said("take").len(),
        2,
        "asked once a mount: {:#?}",
        again.requests
    );
    let unsure_host = ResetHost::refusing(ResetNo::NotQualifying);
    *unsure_host.again.lock().unwrap() = vec![1, 2];
    let first = ResetWorld::new(&RESET_ROWS, 2, &unsure_host.shared).dialog();
    let mut unsure = reset_mock(vec![first, idle_screen()], &RESET_ROWS, 2, &unsure_host);
    let unsure_run = reset_watch(
        "again-again",
        &mut unsure,
        reset_opts(&unsure_host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(unsure_host.said("take").len(), 3, "{:#?}", unsure.requests);
    let waiting = unsure_run
        .of_rule()
        .into_iter()
        .filter(|l| l.starts_with("WAITING"))
        .count();
    assert_eq!(waiting, 2, "once a mount: {:#?}", unsure_run.journal);
}

/// The loop's own gates, each shut before the host is asked: `resume_limits`
#[test]
fn a_gate_of_the_loops_shut_passes_the_reset_over_before_the_host_is_asked() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.resume_limits = false;
    let run = reset_watch("gate-resume", &mut m, opts, &fresh, |_| {});
    assert!(host.said("").is_empty(), "never asked");
    assert_eq!(reset_world(&mut m).chosen, [RESET_WAIT]);
    let said: Vec<&str> = run.of_rule();
    assert_eq!(said.len(), 1, "{:#?}", run.journal);
    assert!(said[0].ends_with("rule=limit-reset@v1 gate=resume-limits"));

    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let mut opts = reset_opts(&host, true, 120);
    opts.idle_host = None;
    let run = reset_watch("gate-host", &mut m, opts, &fresh, |_| {});
    assert_eq!(reset_world(&mut m).chosen, [RESET_WAIT]);
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    let said: Vec<&str> = run.of_rule();
    assert_eq!(said.len(), 1, "{:#?}", run.journal);
    assert!(said[0].ends_with("rule=limit-reset@v1 gate=no-host"));
}

/// A spend is said, whatever it left standing: after the whole path, a host
#[test]
fn a_walled_or_countless_spend_is_said_and_booked_as_used() {
    let walled = ResetOutcome::Walled {
        kind: aterm_phase::ResetKind::Fable,
        left: 0,
    };
    let countless = ResetOutcome::Spent {
        kind: aterm_phase::ResetKind::Weekly,
        asks: false,
        left: None,
        cleared_at: TEST_NOW,
    };
    for (tag, outcome, words, typed) in [
        (
            "walled",
            walled,
            "used (no Fable resets left), the wall still stands",
            1,
        ),
        ("countless", countless, "used (weekly reset)", 0),
    ] {
        let host = ResetHost::reading(outcome);
        let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
        let run = reset_watch(tag, &mut m, reset_opts(&host, true, 120), &fresh, |_| {});
        assert_eq!(
            reset_keys(&m),
            ["down@dialog", "enter@dialog", "enter@confirm"],
            "{tag}"
        );
        let said = format!("rule=limit-reset@v1 {words}");
        assert!(
            run.lines
                .iter()
                .any(|l| l.starts_with("RESET seq=") && l.ends_with(&said)),
            "{tag}: said, not only noted: {:#?}",
            run.lines
        );
        assert_eq!(
            run.rows().last().map(String::as_str),
            Some("waiting|limit reset|ended: used"),
            "{tag}"
        );
        assert_eq!(run.typed().len(), typed, "{tag}: {:#?}", run.ledger);
        if typed > 0 {
            assert!(run.typed()[0].contains("usage-resume@v1"), "{tag}");
        }
        assert_eq!(count(&m, "send"), 0, "{tag}");
        assert_eq!(m.attention, None, "{tag}");
        assert_eq!(reset_violations(&m, &host), Vec::<String>::new(), "{tag}");
    }
}

/// The early-use confirmation — the vendor's line that the limit is not
#[test]
fn the_early_use_confirmation_is_escaped_and_one_continuation_is_owed() {
    use aterm_phase::prompt::fixtures::screen;
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm = screen(aterm_phase::reset::fixtures::CONFIRM_EARLY);
    reset_world(&mut m).exact = false;
    let run = reset_watch(
        "early",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"]
    );
    let presses = m.presses();
    assert_eq!(
        presses.last().map(|r| r.as_str()),
        Some(
            format!(
                "key if-human=0 if={} esc",
                guard_of("   Use a weekly reset?")
            )
            .as_str()
        ),
        "{presses:#?}"
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=inexact-early offer=None last=false"]
    );
    assert_eq!(host.said("take").len(), 1);
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until {}", host.until),
            "declined|Use a weekly reset?|inexact-early".to_string(),
            "waiting|limit reset|ended: kept".to_string(),
        ]
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("kept (inexact-early)")),
        "{:#?}",
        run.journal
    );
    assert!(!run.lines.iter().any(|l| l.starts_with("RESET")));
    let continued: Vec<&String> = run
        .lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(continued.len(), 1, "{:#?}", run.lines);
    assert!(
        continued[0].contains("rule=usage-resume@v1"),
        "{continued:#?}"
    );
    assert_eq!(run.typed().len(), 1, "{:#?}", run.ledger);
    assert!(run.typed()[0].contains("usage-resume@v1"));
    assert_eq!(count(&m, "send"), 0);
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// The focus on no: a confirmation whose cursor is not on its yes is
#[test]
fn a_confirmation_with_the_focus_on_no_is_escaped_never_arrowed() {
    use aterm_phase::prompt::fixtures::screen;
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm = screen(aterm_phase::reset::fixtures::CONFIRM_FOCUS_NO);
    let run = reset_watch(
        "focus-no",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(
        host.said("told"),
        [format!(
            "told entered=false kept=focus offer=weekly 2 Some({}) last=false",
            placed("Oct 5 at 3pm")
        )]
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A kind drawn beside the row while the arrows move: the full reset, the one
#[test]
fn a_kind_drawn_beside_the_row_as_the_arrow_lands_is_never_entered() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::reset::fixtures as f;
    const FULL: &str = "Use a full reset (1 left, until Oct 5)";
    const FABLE: &str =
        "Use a Fable reset (1 left, until Oct 6) · frees up only what Fable used this week";
    let rows = [RESET_STOP, FULL, RESET_WAIT, RESET_CREDITS];
    let world = |host: &ResetHost, drawn: Option<(usize, &str)>| {
        let mut m = reset_mock(vec![], &rows, 0, host);
        let world = reset_world(&mut m);
        world.grant = FULL.to_string();
        world.confirm = screen(f::CONFIRM_FULL);
        world.drawn_at_arrow = drawn.map(|(at, label)| (at, label.to_string()));
        m
    };
    for (tag, at, label, labels) in [
        (
            "weekly-above",
            1,
            RESET_GRANT,
            [RESET_STOP, RESET_GRANT, FULL, RESET_WAIT, RESET_CREDITS],
        ),
        (
            "fable-under",
            2,
            FABLE,
            [RESET_STOP, FULL, FABLE, RESET_WAIT, RESET_CREDITS],
        ),
    ] {
        let host = ResetHost::reading(spent());
        let mut m = world(&host, Some((at, label)));
        let run = reset_watch(tag, &mut m, reset_opts(&host, true, 120), &fresh, |_| {});
        assert_eq!(
            reset_world(&mut m).labels,
            labels,
            "{tag}: the row was drawn"
        );
        assert_eq!(
            reset_world(&mut m).chosen,
            [RESET_WAIT],
            "{tag}: {:#?}",
            run.journal
        );
        assert_eq!(reset_world(&mut m).yes_taken, 0, "{tag}");
        assert!(
            reset_keys(&m).iter().all(|k| k.ends_with("@dialog")),
            "{tag}: {:?}",
            reset_keys(&m)
        );
        assert_eq!(
            host.said("told"),
            ["told entered=false kept=not-opened offer=None last=true"],
            "{tag}: {:#?}",
            run.journal
        );
        assert!(
            run.rows().iter().all(|r| !r.starts_with("approved|")),
            "{tag}: {:#?}",
            run.rows()
        );
        assert_eq!(reset_violations(&m, &host), Vec::<String>::new(), "{tag}");
    }

    let host = ResetHost::reading(spent());
    let mut m = world(&host, None);
    let run = reset_watch(
        "none-drawn",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).chosen, [FULL]);
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert!(
        run.rows()
            .iter()
            .any(|r| r.starts_with("approved|Yes, use my full reset|")),
        "{:#?}",
        run.rows()
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// Under the owner's order (`[harness] limit_reset_kinds`), the row's press
#[test]
fn under_the_owners_order_the_press_reads_the_dialog_again_by_that_order() {
    use aterm_phase::ResetKind::{Full, Weekly};
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::reset::fixtures as f;
    const FULL: &str = "Use a full reset (1 left, until Oct 5)";
    const SESSION: &str = "Use a 5-hour reset (2 left, until Oct 5)";
    let world = |host: &ResetHost, rows: &[&str], drawn: bool| {
        let mut m = reset_mock(vec![], rows, 0, host);
        let world = reset_world(&mut m);
        world.grant = FULL.to_string();
        world.confirm = screen(f::CONFIRM_FULL);
        world.drawn_at_arrow = drawn.then(|| (1, RESET_GRANT.to_string()));
        m
    };
    let listed = |host: &std::sync::Arc<ResetHost>, order: Vec<aterm_phase::ResetKind>| {
        let mut opts = reset_opts(host, true, 120);
        opts.policy.limit_reset_kinds = order;
        opts
    };
    let host = ResetHost::reading(spent());
    let mut m = world(&host, &[RESET_STOP, FULL, RESET_WAIT, RESET_CREDITS], true);
    let run = reset_watch(
        "full-first",
        &mut m,
        listed(&host, vec![Full, Weekly]),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_world(&mut m).labels,
        [RESET_STOP, RESET_GRANT, FULL, RESET_WAIT, RESET_CREDITS],
        "the row was drawn"
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(host.said("take").len(), 2, "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).chosen, [FULL]);
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(spent());
    let mut m = world(
        &host,
        &[RESET_STOP, FULL, SESSION, RESET_WAIT, RESET_CREDITS],
        true,
    );
    let run = reset_watch(
        "weekly-first",
        &mut m,
        listed(&host, vec![Weekly, Full]),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_world(&mut m).labels,
        [
            RESET_STOP,
            RESET_GRANT,
            FULL,
            SESSION,
            RESET_WAIT,
            RESET_CREDITS
        ],
        "the row was drawn"
    );
    assert!(
        !reset_world(&mut m).chosen.iter().any(|c| c == FULL),
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0, "{:#?}", run.journal);

    for (order, chosen, yes) in [(vec![Full], FULL, 1), (vec![], RESET_WAIT, 0)] {
        let host = ResetHost::reading(spent());
        let mut m = world(
            &host,
            &[RESET_STOP, FULL, SESSION, RESET_WAIT, RESET_CREDITS],
            false,
        );
        let run = reset_watch(
            "two-on-offer",
            &mut m,
            listed(&host, order.clone()),
            &fresh,
            |_| {},
        );
        assert_eq!(
            reset_world(&mut m).chosen,
            [chosen],
            "{order:?}: {:#?}",
            run.journal
        );
        assert_eq!(reset_world(&mut m).yes_taken, yes, "{order:?}");
        assert_eq!(
            reset_violations(&m, &host),
            Vec::<String>::new(),
            "{order:?}"
        );
    }
}

/// The claim is asked about again before the Enter, and anything but the
#[test]
fn a_claim_that_is_no_longer_the_sessions_keeps_the_reset() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    *host.lapses_at.lock().unwrap() = Some(2);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_watch(
        "claim-lapsed",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"]
    );
    assert_eq!(host.said("take").len(), 2);
    assert!(
        host.said("told")[0].starts_with("told entered=false kept=claim "),
        "{:?}",
        host.said("told")
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);

    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_watch(
        "claim-old",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| {
            s.set_reset_waits(reset_waits_cut(|w| w.enter_by = Duration::ZERO));
        },
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"]
    );
    assert!(host.said("told")[0].starts_with("told entered=false kept=claim "));
    assert_eq!(reset_world(&mut m).yes_taken, 0);
}

/// A person, three ways, and in none of them is a key of the reset's
#[test]
fn a_person_holds_every_key_of_the_reset() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    m.human_ms = Some(500);
    reset_world(&mut m).exits_when = Some(ResetExit::Statuses(1));
    let run = reset_watch(
        "person-before",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert!(host.said("").is_empty(), "no claim is asked for");
    assert_eq!(m.presses(), Vec::<&String>::new(), "{:#?}", run.journal);
    assert!(run.journal.iter().any(|l| l.starts_with("HELD ")));

    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).person_at_read = Some(1);
    reset_world(&mut m).grace = Duration::from_secs(1);
    let run = reset_watch(
        "person-settle",
        &mut m,
        reset_opts(&host, true, 1),
        &fresh,
        |_| {},
    );
    let held: Vec<&String> = run
        .journal
        .iter()
        .filter(|l| l.starts_with("HELD ") && l.ends_with("(the reset confirmation's key)"))
        .collect();
    assert_eq!(held.len(), 1, "{:#?}", run.journal);
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).person_at_confirm_key = 1;
    reset_world(&mut m).grace = Duration::from_secs(1);
    let run = reset_watch(
        "person-enter",
        &mut m,
        reset_opts(&host, true, 1),
        &fresh,
        |_| {},
    );
    let enters: Vec<(bool, bool)> = reset_world(&mut m)
        .probes
        .iter()
        .filter(|p| p.at == ResetAt::Confirm)
        .map(|p| (p.written, p.hand))
        .collect();
    assert_eq!(
        enters,
        [(false, true), (true, false)],
        "the first skipped under their hand, the second written past it: {:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(m.person_skipped.len(), 1);
    assert_eq!(host.said("told").len(), 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A pending line is waited on, never keyed — but `Checking for a reset…`
#[test]
fn a_pending_line_is_never_keyed_but_checking_past_its_bound() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).check_reads = usize::MAX;
    let run = reset_watch(
        "checking",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.check = Duration::from_millis(60))),
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@checking"],
        "{:#?}",
        run.journal
    );
    let presses = m.presses();
    assert_eq!(
        presses.last().map(|r| r.as_str()),
        Some(
            format!(
                "key if-human=0 if={} esc",
                guard_of("   Checking for a reset…")
            )
            .as_str()
        )
    );
    assert_eq!(
        host.said("told").last().map(String::as_str),
        Some("told entered=false kept=check-timeout offer=None last=false")
    );
    assert_eq!(
        run.rows().last().map(String::as_str),
        Some("waiting|limit reset|ended: kept")
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(ResetOutcome::Unread);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).claim_reads = usize::MAX;
    reset_world(&mut m).exits_when = Some(ResetExit::Told);
    let run = reset_watch(
        "resetting",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.claim = Duration::from_millis(60))),
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).at, ResetAt::Resetting);
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(
        told[0].starts_with("told entered=true kept=- ") && told[0].ends_with(" last=true"),
        "{told:#?}"
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("unread")),
        "{:#?}",
        run.journal
    );
}

/// No Enter follows an Escape. The confirmation's cursor is on its no: the
#[test]
fn no_enter_follows_an_escape_whatever_the_confirmation_then_reads() {
    use aterm_phase::prompt::fixtures::screen;
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm = screen(aterm_phase::reset::fixtures::CONFIRM_FOCUS_NO);
    reset_world(&mut m).esc_drops = 1;
    reset_world(&mut m).cursor_moves_at_esc = true;
    let run = reset_watch(
        "esc-then-yes",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert!(
        reset_world(&mut m).on_yes(),
        "the cursor moved onto the yes"
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert!(
        host.said("told")[0].starts_with("told entered=false kept=focus "),
        "{:?}",
        host.said("told")
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// The loop's stop, set in every wait of the whole path in turn: no wait
#[test]
fn a_stop_in_any_wait_of_the_step_ends_it_with_nothing_written_after() {
    let host = ResetHost::reading(spent());
    let opts = SuperviseOpts {
        max: UNBOUNDED,
        ..reset_opts(&host, true, 120)
    };
    let armed = stop_in_every_wait("reset-step", &opts, None, || {
        for n in [&host.shared.claim, &host.shared.ph] {
            n.store(0, Ordering::SeqCst);
        }
        host.shared.own.store(false, Ordering::SeqCst);
        reset_mock(vec![], &RESET_ROWS, 0, &host)
    });
    assert!(armed.len() > 6, "{armed:#?}");
}

/// The session ledger's row of a grant row an earlier loop took, as it
fn taken_row(ledger: &Path) {
    let row = approvals::Row {
        rule_id: "limit-reset@v1",
        outcome: approvals::Outcome::Approved,
        command: RESET_GRANT,
        reason: "taken: wall until 1790265300",
        box_seq: 7,
    };
    let now = crate::supervise::journal::unix_ms();
    std::fs::write(ledger, format!("{}\n", row.to_json(now, None))).expect("the ledger");
}

/// A world that opens on the plain confirmation: what a loop finds that did
fn at_the_confirmation(host: &Arc<ResetHost>, confirm: Option<Vec<String>>) -> Mock {
    let mut m = reset_mock(vec![], &RESET_ROWS, 1, host);
    let world = reset_world(&mut m);
    if let Some(confirm) = confirm {
        world.confirm = confirm;
        world.exact = false;
    }
    world.at = ResetAt::Confirm;
    world.mounted = Some(Instant::now());
    world.grace = Duration::from_secs(1);
    let frame = world.frame();
    let last = m.screens.len() - 1;
    m.screens[last] = frame;
    m
}

/// A confirmation of another kind than the row taken is kept: the 5-hour row
#[test]
fn a_confirmation_of_another_kind_than_the_row_is_kept() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::reset::fixtures as composed;
    const SESSION: &str = "Use a 5-hour reset (2 left, until Oct 5)";
    let rows = [RESET_STOP, SESSION, RESET_WAIT, RESET_CREDITS];
    let world = |host: &Arc<ResetHost>, confirm: &str, exact: bool| {
        let mut m = reset_mock(vec![], &rows, 0, host);
        let w = reset_world(&mut m);
        w.grant = SESSION.to_string();
        w.confirm = screen(confirm);
        w.exact = exact;
        m
    };

    let host = ResetHost::reading_at(spent(), &["session", "weekly"]);
    let mut m = world(&host, composed::CONFIRM_SESSION, true);
    let run = reset_watch(
        "own-kind",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(reset_world(&mut m).chosen, [SESSION]);
    let takes = host.said("take");
    assert!(
        !takes.is_empty()
            && takes
                .iter()
                .all(|t| t.starts_with("take kind=session left=2 ")),
        "{takes:?}"
    );
    assert!(
        run.rows()
            .iter()
            .any(|r| r.starts_with("approved|Yes, use my 5-hour reset|Refills your session")),
        "{:#?}",
        run.rows()
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading_at(ResetOutcome::Kept, &["session", "weekly"]);
    let mut m = world(&host, composed::CONFIRM, false);
    let run = reset_watch(
        "other-kind",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0, "no reset spent");
    assert!(
        run.rows()
            .contains(&"declined|Use a weekly reset?|kind".to_string()),
        "{:#?}",
        run.rows()
    );
    let told = host.said("told");
    assert!(
        told.len() == 1 && told[0].starts_with("told entered=false kept=kind "),
        "{told:?}"
    );
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A host whose key is off under a world an earlier loop left its step in:
fn key_off_after(m: &mut Mock, host: &ResetHost, ph: i64) {
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(ph, Ordering::SeqCst);
    reset_world(m).untold = true;
}

/// What the `declined` row of a successor's Escape names on the world's
fn declined_as(m: &mut Mock) -> String {
    reset_world(m)
        .confirm
        .iter()
        .find(|row| aterm_phase::reset_title(row).is_some())
        .map_or_else(
            || "the reset confirmation".to_string(),
            |row| row.trim().to_string(),
        )
}

/// What a key-off successor's step left, the same at every confirmation it
fn assert_ended_untold(tag: &str, m: &mut Mock, host: &ResetHost, run: &ResetRun) {
    assert_eq!(reset_keys(m), ["esc@confirm"], "{tag}: {:#?}", run.journal);
    assert_eq!(reset_world(m).yes_taken, 0, "{tag}");
    assert!(host.said("").is_empty(), "{tag}: {:?}", host.said(""));
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            format!("declined|{}|successor", declined_as(m)),
            "waiting|limit reset|ended: untold (key off)".to_string(),
        ],
        "{tag}"
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("ended untold (limit_reset off)")),
        "{tag}: {:#?}",
        run.journal
    );
    assert_eq!(run.typed(), Vec::<&str>::new(), "{tag}: {:#?}", run.ledger);
    assert!(
        !run.journal
            .iter()
            .any(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED")),
        "{tag}: {:#?}",
        run.journal
    );
    assert_eq!((count(m, "send"), count(m, "turn")), (0, 0), "{tag}");
    assert_eq!(m.attention, None, "{tag}: nothing asked of the owner");
    assert_eq!(reset_violations(m, host), Vec::<String>::new(), "{tag}");
}

/// The successor: a loop that starts on a ledger whose last row of the rule
#[test]
fn a_successor_escapes_the_confirmation_and_never_enters_it() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(1, Ordering::SeqCst);
    let mut m = at_the_confirmation(&host, None);
    let run = reset_watch(
        "successor",
        &mut m,
        reset_opts(&host, true, 1),
        &taken_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["esc@confirm"], "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(
        host.said("told"),
        [format!(
            "told entered=false kept=successor offer=weekly 2 Some({}) last=false",
            placed("Oct 5 at 3pm")
        )]
    );
    assert!(host.said("take").is_empty(), "a successor asks for no go");
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|Use a weekly reset?|successor".to_string(),
            "waiting|limit reset|ended: kept".to_string(),
        ]
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, None);
    let mut opts = reset_opts(&host, true, 1);
    opts.policy.continue_policy = false;
    let run = reset_watch("persons", &mut m, opts, &fresh, |_| {});
    assert_eq!(
        reset_keys(&m),
        ["down@confirm", "enter@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert!(host.said("").is_empty(), "no step, nothing told");
    assert!(run.rows().is_empty());
    assert!(
        run.ledger.iter().any(|r| r.contains("no-spend@v1")),
        "{:#?}",
        run.ledger
    );

    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, None);
    key_off_after(&mut m, &host, 1);
    let run = reset_watch(
        "key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_ended_untold("key-off", &mut m, &host, &run);
}

/// A successor's first look meets a box with nothing of the confirmation
#[test]
fn a_successor_escapes_a_box_with_nothing_of_the_confirmation_left() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(1, Ordering::SeqCst);
    let mut m = at_the_confirmation(&host, Some(nothing_left_head_off()));
    let run = reset_watch(
        "successor-nothing-left",
        &mut m,
        reset_opts(&host, true, 1),
        &taken_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["esc@confirm"], "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=successor offer=None last=false"]
    );
    assert!(host.said("take").is_empty(), "a successor asks for no go");
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|the reset confirmation|successor".to_string(),
            "waiting|limit reset|ended: kept".to_string(),
        ]
    );
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, Some(nothing_left_head_off()));
    key_off_after(&mut m, &host, 1);
    let run = reset_watch(
        "successor-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_ended_untold("successor-key-off", &mut m, &host, &run);
}

/// The weekly reset's confirmation in Claude Code 2.1.304's screen-reader
fn screen_reader_confirmation() -> Vec<String> {
    let rows = confirm_frame(&[
        "   Use a weekly reset?",
        "",
        "   Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am",
        "   2 weekly resets left · use by Oct 5 at 3pm",
        "",
        "   y. Yes, use my weekly reset",
        "   n. No, keep it",
        "   Enter y/n: ",
        "",
        "   Enter to confirm · Esc to cancel",
    ]);
    let read = aterm_phase::read(Some("claude"), &rows, None);
    assert!(read.prompt.is_none(), "no box is read: {:#?}", read.prompt);
    rows
}

/// A confirmation with no box the reader takes, its title still up, is the
#[test]
fn a_successor_escapes_a_titled_confirmation_with_no_box_until_its_enter_is_written() {
    let frame = screen_reader_confirmation();
    let title = frame
        .iter()
        .find(|row| aterm_phase::reset_title(row).is_some())
        .cloned()
        .expect("the title row");
    let host = ResetHost::reading(ResetOutcome::Kept);
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(1, Ordering::SeqCst);
    let mut m = at_the_confirmation(&host, Some(frame.clone()));
    let run = reset_watch(
        "screen-reader",
        &mut m,
        reset_opts(&host, true, 1),
        &taken_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["esc@confirm"], "{:#?}", run.journal);
    assert_eq!(
        m.presses().last().map(|r| ungen(r)),
        Some(format!("key if-human=0 if={} esc", guard_of(&title))),
        "the guard of the title row"
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=successor offer=None last=false"]
    );
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|Use a weekly reset?|successor".to_string(),
            "waiting|limit reset|ended: kept".to_string(),
        ]
    );
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let entered_row = |ledger: &Path| {
        taken_row(ledger);
        let yes = aterm_phase::ResetKind::Weekly.yes();
        let row = approvals::Row {
            rule_id: "limit-reset@v1",
            outcome: approvals::Outcome::Approved,
            command: &yes,
            reason: RESET_HEAD,
            box_seq: 9,
        };
        let now = crate::supervise::journal::unix_ms();
        let mut body = std::fs::read_to_string(ledger).expect("the ledger");
        body.push_str(&format!("{}\n", row.to_json(now, None)));
        std::fs::write(ledger, body).expect("the ledger");
    };
    let host = ResetHost::reading(ResetOutcome::Kept);
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(2, Ordering::SeqCst);
    let mut m = at_the_confirmation(&host, Some(frame));
    let mut opts = reset_opts(&host, true, 1);
    opts.max = Duration::from_secs(5);
    let run = reset_watch("screen-reader-entered", &mut m, opts, &entered_row, |_| {});
    assert_eq!(reset_keys(&m), Vec::<String>::new(), "{:#?}", run.journal);
    assert_eq!(m.presses(), Vec::<&String>::new());
    assert!(reset_world(&mut m).reads > 1, "the frame was read again");
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(m.attention, None);
}

/// The yes full power would take: a key-off successor's first look meets the
#[test]
fn a_key_off_successor_escapes_a_reworded_consent_and_never_takes_its_yes() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, Some(nothing_left_consent()));
    key_off_after(&mut m, &host, 1);
    let run = reset_watch(
        "consent-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_ended_untold("consent-key-off", &mut m, &host, &run);

    let ended = |how: &'static str| {
        move |ledger: &Path| {
            taken_row(ledger);
            let row = approvals::Row {
                rule_id: "limit-reset@v1",
                outcome: approvals::Outcome::Waiting,
                command: "limit reset",
                reason: how,
                box_seq: 8,
            };
            let now = crate::supervise::journal::unix_ms();
            let other = approvals::Row {
                rule_id: "limit-reset@v1",
                outcome: approvals::Outcome::Approved,
                command: RESET_GRANT,
                reason: "taken: wall until 1790265300",
                box_seq: 3,
            };
            let mut body = std::fs::read_to_string(ledger).expect("the ledger");
            body.push_str(&format!("{}\n", row.to_json(now, None)));
            body.push_str(&format!("{}\n", other.to_json(now, Some("@s-9"))));
            std::fs::write(ledger, body).expect("the ledger");
        }
    };
    type Seed = Box<dyn Fn(&Path)>;
    let seeds: [(&str, Seed); 4] = [
        ("consent-no-row", Box::new(fresh)),
        (
            "consent-ended-untold",
            Box::new(ended("ended: untold (key off)")),
        ),
        ("consent-ended-stale", Box::new(ended("ended: stale"))),
        ("consent-ended-kept", Box::new(ended("ended: kept"))),
    ];
    let mut seen: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for (tag, seed) in &seeds {
        let host = ResetHost::reading(ResetOutcome::Kept);
        let mut m = at_the_confirmation(&host, Some(nothing_left_consent()));
        reset_world(&mut m).exits_when = Some(ResetExit::Spent);
        let mut opts = reset_opts(&host, false, 1);
        opts.policy.continue_policy = false;
        let before = {
            let dir = crate::supervise::test_scratch_path("reset", &format!("{tag}-seed"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("tmp dir");
            let at = dir.join("s-1.jsonl");
            seed(&at);
            let rows = std::fs::read_to_string(&at).unwrap_or_default();
            let _ = std::fs::remove_dir_all(&dir);
            rows.lines().count()
        };
        let run = reset_watch(tag, &mut m, opts, seed.as_ref(), |_| {});
        assert_eq!(
            reset_keys(&m),
            ["enter@confirm"],
            "{tag}: {:#?}",
            run.journal
        );
        let presses: Vec<String> = m.presses().iter().map(|r| ungen(r)).collect();
        assert_eq!(
            presses,
            [format!(
                "key if-gen=* if={} enter",
                guard_of("   ❯ Continue with a refill")
            )],
            "{tag}: {:#?}",
            m.requests
        );
        assert_eq!(reset_world(&mut m).yes_taken, 1, "{tag}");
        assert!(host.said("").is_empty(), "{tag}: nothing asked or told");
        assert!(run.of_rule().is_empty(), "{tag}: {:#?}", run.journal);
        let added: Vec<&String> = run.ledger[before..].iter().collect();
        assert!(
            !added.iter().any(|r| r.contains("limit-reset@v1")),
            "{tag}: {added:#?}"
        );
        assert!(
            added
                .iter()
                .any(|r| r.contains("allow-once@v1") && r.contains("approved")),
            "{tag}: {:#?}",
            run.ledger
        );
        let unstamped = added
            .iter()
            .map(|r| {
                r.split_once(",\"sid\"")
                    .map_or("", |(_, rest)| rest)
                    .to_string()
            })
            .collect();
        seen.push((presses, unstamped));
    }
    for (n, other) in seen.iter().enumerate().skip(1) {
        assert_eq!(other, &seen[0], "{}", seeds[n].0);
    }
}

/// `Resetting…` met by a key-off successor — the earlier loop wrote the
#[test]
fn a_key_off_successor_waits_out_resetting_and_owes_nothing() {
    let entered_row = |ledger: &Path| {
        taken_row(ledger);
        let row = approvals::Row {
            rule_id: "limit-reset@v1",
            outcome: approvals::Outcome::Approved,
            command: approvals::RESET_YES_2_1_287,
            reason: RESET_HEAD,
            box_seq: 9,
        };
        let now = crate::supervise::journal::unix_ms();
        let mut body = std::fs::read_to_string(ledger).expect("the ledger");
        body.push_str(&format!("{}\n", row.to_json(now, None)));
        std::fs::write(ledger, body).expect("the ledger");
    };
    let at_resetting = |host: &Arc<ResetHost>| {
        let mut m = at_the_confirmation(host, None);
        let world = reset_world(&mut m);
        world.at = ResetAt::Resetting;
        let frame = world.frame();
        let last = m.screens.len() - 1;
        m.screens[last] = frame;
        m
    };
    let failed = ResetOutcome::Failed { login: false };
    let rows = |end: &str| {
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            format!("approved|{}|{RESET_HEAD}", approvals::RESET_YES_2_1_287),
            format!("waiting|limit reset|ended: {end}"),
        ]
    };

    let host = ResetHost::reading(failed);
    let mut m = at_resetting(&host);
    key_off_after(&mut m, &host, 2);
    let run = reset_watch(
        "resetting-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &entered_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), Vec::<String>::new(), "{:#?}", run.journal);
    assert_eq!(m.presses(), Vec::<&String>::new());
    assert!(host.said("").is_empty(), "{:?}", host.said(""));
    assert_eq!(run.rows(), rows("untold (key off)"));
    assert!(
        run.of_rule().iter().any(|l| l.starts_with("RESET seq=")
            && l.ends_with("ended untold (limit_reset off, its Enter was written)")),
        "{:#?}",
        run.journal
    );
    assert_eq!(run.typed(), Vec::<&str>::new(), "{:#?}", run.ledger);
    assert!(
        !run.journal
            .iter()
            .any(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED")),
        "{:#?}",
        run.journal
    );
    assert_eq!((count(&m, "send"), count(&m, "turn")), (0, 0));
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(failed);
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(2, Ordering::SeqCst);
    let mut m = at_resetting(&host);
    let run = reset_watch(
        "resetting-key-on",
        &mut m,
        reset_opts(&host, true, 1),
        &entered_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), Vec::<String>::new(), "{:#?}", run.journal);
    assert_eq!(
        host.said(""),
        ["told entered=true kept=- offer=None last=false"]
    );
    assert_eq!(run.rows(), rows("failed"));
    assert_eq!(run.typed().len(), 1, "{:#?}", run.ledger);
    assert!(run.typed()[0].contains("usage-resume@v1"));
}

/// The row's press got no answer: its Enter was written and taken, and the
#[test]
fn a_row_press_with_no_answer_is_closed_and_never_entered() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reply_to = Some((ResetAt::Dialog, "enter"));
    reset_world(&mut m).grace = Duration::from_secs(1);
    let run = reset_watch(
        "lost-press",
        &mut m,
        reset_opts(&host, true, 1),
        &fresh,
        |s| {
            s.set_reconnect(Duration::from_secs(30));
            s.setup.pause = Duration::from_millis(5);
            s.setup.pause_max = Duration::from_millis(20);
        },
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert!(
        host.said("told")[0].starts_with("told entered=false kept=successor "),
        "{:?}",
        host.said("told")
    );
    assert_eq!(
        run.rows().first().map(String::as_str),
        Some(format!("approved|{RESET_GRANT}|taken: the press's answer never came").as_str())
    );
    assert_eq!(
        run.rows().last().map(String::as_str),
        Some("waiting|limit reset|ended: kept")
    );
}

/// A day and hour of the vendor's is placed in the wall notice's zone where
#[test]
fn a_deadline_is_placed_in_the_wall_notices_zone_where_the_loop_holds_one() {
    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let local = s.reset_place("Oct 5 at 3pm").expect("placed");
    assert_eq!(local, placed("Oct 5 at 3pm"));
    assert_eq!(local % 86_400, 22 * 3600, "3pm PDT is 22:00Z");
    assert_eq!(
        s.reset_place("Oct 5 at 12am"),
        Some(local - 15 * 3600),
        "the day's first second"
    );
    assert_eq!(
        s.reset_place("Jan 2, 2027 at 9:30am").map(|at| at % 86_400),
        Some(16 * 3600 + 30 * 60),
        "another year's"
    );
    let tomorrow = s.reset_place("Sep 18 at 3pm").expect("placed");
    assert!(
        (0..2 * 86_400).contains(&(tomorrow - TEST_NOW)),
        "{tomorrow}"
    );
    assert_eq!(
        s.reset_place("Sep 18, 2027 at 3pm"),
        Some(tomorrow + 365 * 86_400),
        "the year it names"
    );
    assert_eq!(
        s.reset_place("Sep 18, 2026 at 3pm"),
        Some(tomorrow),
        "this year, named"
    );
    for not in ["3pm", "in 3h", "Oct 5", "soon"] {
        assert_eq!(s.reset_place(not), None, "{not}");
    }
    s.set_local_lookup(|unix| (unix > TEST_NOW + 10 * 86_400).then_some(PDT - 3600));
    assert_eq!(s.reset_place("Oct 5 at 3pm"), Some(local + 3600));
    assert_eq!(s.reset_place("Sep 18 at 3pm"), Some(tomorrow));
    s.set_local_lookup(|_| None);
    s.limit = Some(Episode {
        seq: 1,
        resumes_itself: false,
        reset_unix: None,
        reset_text: "Oct 9 at 5am (UTC)".to_string(),
        extended: false,
        raised: false,
    });
    assert_eq!(s.reset_place("Oct 5 at 3pm"), Some(local - 7 * 3600));
}

/// The composed confirmation's frame above its dialog, and `body` under it:
fn confirm_frame(body: &[&str]) -> Vec<String> {
    use aterm_phase::prompt::fixtures::screen;
    let mut rows: Vec<String> = screen(aterm_phase::reset::fixtures::CONFIRM)[..3].to_vec();
    rows.extend(body.iter().map(|row| (*row).to_string()));
    rows
}

/// The composed confirmation with nothing of it left — its body and both
fn nothing_left_head_off() -> Vec<String> {
    let rows: Vec<String> = [
        "   This takes effect at once",
        "   Nothing is lost by waiting",
        "",
        "   ❯ Yes",
        "     No",
        "",
        "   Enter to confirm · Esc to cancel",
    ]
    .iter()
    .map(|row| (*row).to_string())
    .collect();
    let read = aterm_phase::read(Some("claude"), &rows, None).prompt;
    assert!(
        read.as_ref()
            .is_some_and(|p| p.head_off_screen && p.kind == PromptKind::Other),
        "a box of no known kind, its head off the pane: {read:#?}"
    );
    rows
}

/// The composed confirmation with nothing of it left and its head on the
fn nothing_left_consent() -> Vec<String> {
    use crate::supervise::policy::approval::{ApprovalCtx, Choice, decide};
    let rows = confirm_frame(&[
        "   Limit refill",
        "",
        "   This takes effect at once",
        "",
        "   ❯ Continue with a refill",
        "     Not now",
        "",
        "   Enter to confirm · Esc to cancel",
    ]);
    let reading = aterm_phase::read(Some("claude"), &rows, None);
    let prompt = reading.prompt.as_ref().expect("a box");
    assert!(
        prompt.kind == PromptKind::Other
            && !prompt.head_off_screen
            && prompt.options.iter().all(|o| o.n.is_none()),
        "a box of no known kind, its head read, unnumbered: {prompt:#?}"
    );
    assert_eq!(
        aterm_phase::reset_confirm(prompt, &rows),
        aterm_phase::ConfirmRead::Not
    );
    let ctx = ApprovalCtx::new(
        std::env::temp_dir(),
        None,
        501,
        None,
        std::sync::Arc::new(crate::supervise::policy::LiveProbe),
    );
    match decide(&reading, &rows, &ctx) {
        Decision::Approve {
            rule_id: "allow-once@v1",
            choice: Choice::Focus { steps: 0, label },
            ..
        } => assert_eq!(label, "Continue with a refill"),
        other => panic!("full power takes its yes: {other:#?}"),
    }
    rows
}

/// The Enter is judged on the wall as the host reads it at the Enter, not as
#[test]
fn the_enter_is_judged_on_the_wall_as_the_host_reads_it_then() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    *host.short_from.lock().unwrap() = Some(2);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "short-by-then",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(host.said("take").len(), 2);
    assert!(
        host.said("told")[0].starts_with("told entered=false kept=not-qualifying "),
        "{:?}",
        host.said("told")
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A person who keys after the dialog was judged — after its read and its
#[test]
fn a_person_keying_before_the_rows_first_key_holds_the_row() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).person_at_row_key = 1;
    reset_world(&mut m).grace = Duration::from_secs(1);
    let run = reset_watch(
        "person-row",
        &mut m,
        reset_opts(&host, true, 1),
        &fresh,
        |_| {},
    );
    let at_dialog: Vec<(String, bool, bool)> = reset_world(&mut m)
        .probes
        .iter()
        .filter(|p| p.at == ResetAt::Dialog)
        .map(|p| (p.key.clone(), p.written, p.hand))
        .collect();
    assert_eq!(
        at_dialog,
        [
            ("down".to_string(), false, true),
            ("down".to_string(), true, false),
            ("enter".to_string(), true, false),
        ],
        "{:#?}",
        run.journal
    );
    assert_eq!(m.person_skipped.len(), 1);
    assert!(run.journal.iter().any(|l| l.starts_with("HELD ")));
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(host.said("told").len(), 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A reconnect window for a run whose server drops an exchange.
fn riding_out(s: &mut Watched<'_, '_>) {
    s.set_reconnect(Duration::from_secs(30));
    s.setup.pause = Duration::from_millis(5);
    s.setup.pause_max = Duration::from_millis(20);
}

/// An Enter whose answer never came is on the books: cut mid-exchange, the
#[test]
fn an_enter_whose_answer_never_came_is_on_the_books() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reply_to = Some((ResetAt::Confirm, "enter"));
    let run = reset_watch(
        "lost-enter",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        riding_out,
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until {}", host.until),
            format!("approved|Yes, use my weekly reset|{RESET_HEAD}"),
            "waiting|limit reset|ended: used".to_string(),
        ]
    );
    assert_eq!(
        run.of_rule()
            .iter()
            .filter(|l| l.contains(" confirmed ("))
            .count(),
        1,
        "{:#?}",
        run.journal
    );
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(
        told[0].starts_with("told entered=true kept=- "),
        "{told:#?}"
    );
}

/// A row older than a step can be is ended untold, at either key: a loop that
#[test]
fn a_row_older_than_a_step_can_be_is_escaped_and_ended_untold() {
    let old_row = |ledger: &Path| {
        let row = approvals::Row {
            rule_id: "limit-reset@v1",
            outcome: approvals::Outcome::Approved,
            command: RESET_GRANT,
            reason: "taken: wall until 1790265300",
            box_seq: 7,
        };
        let then = crate::supervise::journal::unix_ms() - 16 * 60 * 1000;
        std::fs::write(ledger, format!("{}\n", row.to_json(then, None))).expect("the ledger");
    };
    for (key, on) in [("on", true), ("off", false)] {
        for (world, confirm) in [("consent", Some(nothing_left_consent())), ("plain", None)] {
            let tag = format!("stale-{world}-key-{key}");
            let host = ResetHost::reading(ResetOutcome::Kept);
            let mut m = at_the_confirmation(&host, confirm);
            host.shared.ph.store(1, Ordering::SeqCst);
            reset_world(&mut m).untold = true;
            let run = reset_watch(&tag, &mut m, reset_opts(&host, on, 1), &old_row, |_| {});
            assert_eq!(reset_keys(&m), ["esc@confirm"], "{tag}: {:#?}", run.journal);
            assert_eq!(reset_world(&mut m).yes_taken, 0, "{tag}");
            assert!(host.said("").is_empty(), "{tag}: {:?}", host.said(""));
            assert_eq!(
                run.rows(),
                [
                    format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
                    format!("declined|{}|successor", declined_as(&mut m)),
                    "waiting|limit reset|ended: stale".to_string(),
                ],
                "{tag}"
            );
            assert!(
                run.of_rule()
                    .iter()
                    .any(|l| l.starts_with("RESET seq=0 ") && l.contains(" stale ")),
                "{tag}: {:#?}",
                run.journal
            );
            assert!(
                run.of_rule()
                    .iter()
                    .any(|l| l.starts_with("RESET seq=") && l.ends_with("ended untold (stale)")),
                "{tag}: {:#?}",
                run.journal
            );
            assert_eq!(run.typed(), Vec::<&str>::new(), "{tag}: {:#?}", run.ledger);
            assert_eq!((count(&m, "send"), count(&m, "turn")), (0, 0), "{tag}");
            assert_eq!(m.attention, None, "{tag}");
            assert_eq!(reset_violations(&m, &host), Vec::<String>::new(), "{tag}");
        }
    }
}

/// `Checking…` met by a key-off successor and standing past its bound gets
#[test]
fn a_key_off_successor_escapes_checking_past_its_bound_and_ends_untold() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, None);
    {
        let world = reset_world(&mut m);
        world.at = ResetAt::Checking;
        world.check_reads = usize::MAX;
        let frame = world.frame();
        let last = m.screens.len() - 1;
        m.screens[last] = frame;
    }
    key_off_after(&mut m, &host, 1);
    let run = reset_watch(
        "checking-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.check = Duration::from_millis(60))),
    );
    assert_eq!(reset_keys(&m), ["esc@checking"], "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert!(host.said("").is_empty(), "{:?}", host.said(""));
    let rows = run.rows();
    assert_eq!(rows.len(), 3, "{rows:#?}");
    assert!(
        rows[1].starts_with("declined|") && rows[1].ends_with("|check-timeout"),
        "{rows:#?}"
    );
    assert_eq!(rows[2], "waiting|limit reset|ended: untold (key off)");
    assert_eq!(run.typed(), Vec::<&str>::new(), "{:#?}", run.ledger);
    assert_eq!((count(&m, "send"), count(&m, "turn")), (0, 0));
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// An Escape that does not take, with the key off: tried again on the same
#[test]
fn a_key_off_successors_escape_that_does_not_take_is_tried_again_and_recorded_once() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, Some(nothing_left_consent()));
    key_off_after(&mut m, &host, 1);
    reset_world(&mut m).esc_drops = 2;
    let run = reset_watch(
        "esc-ladder-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["esc@confirm", "esc@confirm", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    let said = host.said("");
    assert!(
        said.len() == 1 && said[0].starts_with("inform:"),
        "one record: {said:?}"
    );
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|the reset confirmation|successor".to_string(),
            "waiting|limit reset|ended: untold (key off)".to_string(),
        ]
    );
    assert_eq!(run.typed(), Vec::<&str>::new());
    assert_eq!((count(&m, "send"), count(&m, "turn")), (0, 0));
    assert_eq!(m.attention, None, "a record, never a badge");
    assert_eq!(count(&m, "meta set attention"), 0);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// An attended loop (`supervise`) takes no step up, at either key: it never
#[test]
fn an_attended_loop_takes_no_step_up() {
    for on in [false, true] {
        let mut runs: Vec<Vec<String>> = Vec::new();
        for (tag, seed) in [("attended-row", true), ("attended-none", false)] {
            let host = ResetHost::reading(ResetOutcome::Kept);
            let mut m = at_the_confirmation(&host, None);
            reset_world(&mut m).untold = true;
            let dir = crate::supervise::test_scratch_path("reset", &format!("{tag}-{on}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("tmp dir");
            let ledger = dir.join("s-1.jsonl");
            if seed {
                taken_row(&ledger);
            }
            let idle = {
                let mut s = session(&mut m, None);
                s.set_clock(TEST_NOW, PDT);
                s.set_zone_lookup(test_zones);
                s.set_approval_ledger(Some(ledger.clone()));
                let mut log: Vec<u8> = Vec::new();
                let _ = s.supervise_to(&reset_opts(&host, on, 1), &mut log);
                s.reset == ResetStep::Idle
            };
            assert!(idle, "{tag} key={on}: no step is held");
            let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
            assert_eq!(
                rows.lines()
                    .filter(|r| r.contains("limit-reset@v1"))
                    .count(),
                usize::from(seed),
                "{tag} key={on}: {rows}"
            );
            assert!(host.said("").is_empty(), "{tag} key={on}");
            let _ = std::fs::remove_dir_all(&dir);
            runs.push(m.presses().iter().map(|r| ungen(r)).collect());
        }
        assert_eq!(runs[0], runs[1], "key={on}");
    }
}

/// The box the row opened is the step's, whatever it is reworded to. Every
#[test]
fn the_box_the_row_opened_is_the_steps_whatever_it_is_reworded_to() {
    let no_title = confirm_frame(&[
        "   Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am",
        "   2 weekly resets left · use by Oct 5 at 3pm",
        "",
        "   ❯ Sure, use it",
        "     No, keep it",
        "",
        "   Enter to confirm · Esc to cancel",
    ]);
    let nothing_left = confirm_frame(&[
        "   Spend it?",
        "",
        "   This refills what you have run out of",
        "",
        "   ❯ Sure, go ahead",
        "     Not now",
        "",
        "   Enter to confirm · Esc to cancel",
    ]);
    const SESSION: &str = "Use a 5-hour reset (2 left, until Oct 5)";
    let boxes = [
        ("no-title", no_title, 3, 1),
        ("nothing-left", nothing_left, 3, 1),
        ("nothing-left-first-read", nothing_left_head_off(), 0, 0),
    ];
    let grants: [(&str, &[&str], &str); 2] = [
        (RESET_GRANT, &["weekly"], "Use a weekly reset?"),
        (SESSION, &["session", "weekly"], "Use a 5-hour reset?"),
    ];
    for ((tag, frame, first, check_reads), (grant, limits, title)) in boxes
        .iter()
        .flat_map(|b| grants.iter().map(move |g| (b.clone(), *g)))
    {
        let tag = format!("{tag} after {grant}");
        let first = frame[first].clone();
        let host = ResetHost::reading_at(ResetOutcome::Kept, limits);
        let rows = [RESET_STOP, grant, RESET_WAIT, RESET_CREDITS];
        let mut m = reset_mock(vec![], &rows, 0, &host);
        reset_world(&mut m).grant = grant.to_string();
        reset_world(&mut m).confirm = frame;
        reset_world(&mut m).exact = false;
        reset_world(&mut m).check_reads = check_reads;
        let run = reset_watch(&tag, &mut m, reset_opts(&host, true, 120), &fresh, |_| {});
        assert_eq!(
            reset_keys(&m),
            ["down@dialog", "enter@dialog", "esc@confirm"],
            "{tag}: {:#?}",
            run.journal
        );
        let presses = m.presses();
        assert_eq!(
            presses.last().map(|r| r.as_str()),
            Some(format!("key if-human=0 if={} esc", guard_of(&first)).as_str()),
            "{tag}"
        );
        assert_eq!(
            host.said("told"),
            ["told entered=false kept=inexact-title offer=None last=false"],
            "{tag}"
        );
        assert_eq!(reset_world(&mut m).yes_taken, 0, "{tag}");
        assert!(
            run.rows()
                .contains(&format!("declined|{title}|inexact-title")),
            "{tag}: {:#?}",
            run.rows()
        );
        assert_eq!(m.attention, None, "{tag}");
        assert_eq!(reset_violations(&m, &host), Vec::<String>::new(), "{tag}");
    }
}

/// The step never escapes an MCP server dialog: Claude Code saves its Esc
#[test]
fn the_step_never_escapes_an_mcp_server_dialog() {
    use aterm_phase::prompt::fixtures::{
        MCP_SERVER_DERIVED, MCP_SERVER_NARROW, MCP_SERVER_ONE, MCP_SERVER_TWO, screen,
    };
    let one = screen(MCP_SERVER_ONE);
    let two = screen(MCP_SERVER_TWO);
    let rule = two
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the rule");
    let submit = two
        .iter()
        .position(|r| r.trim() == "Enable selected")
        .expect("the submit row");
    let retitle = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| r.replace("new MCP servers found in this project", "new tools found"))
            .collect()
    };
    let half: Vec<String> = two
        .iter()
        .enumerate()
        .map(|(i, r)| if i < submit { r.clone() } else { String::new() })
        .collect();
    let mut mid_move = two.clone();
    mid_move[submit] = "  ❯    Enable selected".to_string();
    let read = |rows: &[String]| {
        aterm_phase::read(Some("claude"), rows, None)
            .prompt
            .expect("a box")
    };
    let p = read(&half[rule + 1..]);
    assert!(
        p.kind == PromptKind::Other
            && p.head_off_screen
            && p.cancel.is_none()
            && p.options.iter().all(|o| o.label.starts_with("[✔]")),
        "{p:#?}"
    );
    let p = read(&retitle(&mid_move));
    assert!(
        p.kind == PromptKind::Other
            && p.options.iter().all(|o| o.role == aterm_phase::Role::Other)
            && p.options.iter().any(|o| o.label == "Enable selected"),
        "{p:#?}"
    );
    let narrow = MCP_SERVER_NARROW
        .iter()
        .chain(&MCP_SERVER_DERIVED)
        .enumerate()
        .map(|(k, text)| {
            let frame = screen(text);
            let boxed = aterm_phase::read(Some("claude"), &frame, None)
                .prompt
                .is_some();
            (format!("narrow-{k}"), frame, !boxed)
        });
    let frames = [
        ("one", one.clone(), false),
        (
            "one-retitled",
            one.iter()
                .map(|r| r.replace("New MCP server found in this project:", "New tool found:"))
                .collect(),
            false,
        ),
        ("two", two.clone(), false),
        ("two-retitled", retitle(&two), false),
        ("two-rule-cut", two[rule + 1..].to_vec(), false),
        ("two-head-cut", two[rule + 3..].to_vec(), false),
        ("two-half-drawn-rule-cut", half[rule + 1..].to_vec(), true),
        ("two-retitled-mid-move", retitle(&mid_move), false),
    ]
    .map(|(name, frame, unread)| (name.to_string(), frame, unread))
    .into_iter()
    .chain(narrow);
    for (name, frame, unread) in frames {
        let boxed = aterm_phase::read(Some("claude"), &frame, None)
            .prompt
            .is_some();
        for (read, check_reads) in [("first-read", 0), ("after-checking", 1)] {
            let tag = format!("mcp-{name}-{read}");
            let host = ResetHost::reading(ResetOutcome::Kept);
            let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
            reset_world(&mut m).confirm = frame.clone();
            reset_world(&mut m).exact = false;
            reset_world(&mut m).check_reads = check_reads;
            reset_world(&mut m).exits_when = Some(if boxed {
                ResetExit::Asked
            } else {
                ResetExit::Told
            });
            let run = reset_watch(&tag, &mut m, reset_opts(&host, true, 120), &fresh, |s| {
                if unread {
                    s.set_reset_waits(reset_waits_cut(|w| w.open = Duration::from_millis(60)));
                }
            });
            assert_eq!(
                reset_keys(&m),
                ["down@dialog", "enter@dialog"],
                "{tag}: {:#?}",
                run.journal
            );
            assert!(
                !m.presses().iter().any(|p| p.ends_with(" esc")),
                "{tag}: {:#?}",
                m.presses()
            );
            assert_eq!(reset_world(&mut m).yes_taken, 0, "{tag}");
            assert_eq!(host.said("inform").len(), usize::from(unread), "{tag}");
            assert_eq!(m.attention.is_some(), boxed, "{tag}: {:#?}", run.journal);
        }
    }
}

/// The step never escapes an answer kept for good by its label under a rest
#[test]
fn the_step_never_escapes_an_answer_kept_for_good_by_its_label() {
    use crate::supervise::policy::approval::tests::always_under_a_lowercase_rest as frame;
    for (label, kept) in [
        ("Continue with tools, always on", true),
        ("Continue with tools, on", false),
    ] {
        for (read, check_reads) in [("first-read", 0), ("after-checking", 1)] {
            let tag = format!("always-{kept}-{read}");
            let host = ResetHost::reading(ResetOutcome::Kept);
            let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
            reset_world(&mut m).confirm = frame(label);
            reset_world(&mut m).exact = false;
            reset_world(&mut m).check_reads = check_reads;
            if kept {
                reset_world(&mut m).exits_when = Some(ResetExit::Asked);
            }
            let run = reset_watch(&tag, &mut m, reset_opts(&host, true, 120), &fresh, |_| {});
            let escaped = m.presses().iter().any(|p| p.ends_with(" esc"));
            assert_eq!(reset_world(&mut m).yes_taken, 0, "{tag}");
            if kept {
                assert_eq!(
                    reset_keys(&m),
                    ["down@dialog", "enter@dialog"],
                    "{tag}: {:#?}",
                    run.journal
                );
                assert!(!escaped, "{tag}: {:#?}", m.presses());
                assert!(m.attention.is_some(), "{tag}: {:#?}", run.journal);
            } else {
                assert_eq!(
                    reset_keys(&m),
                    ["down@dialog", "enter@dialog", "esc@confirm"],
                    "{tag}: {:#?}",
                    run.journal
                );
                assert_eq!(
                    host.said("told"),
                    ["told entered=false kept=inexact-title offer=None last=false"],
                    "{tag}"
                );
                assert_eq!(m.attention, None, "{tag}");
            }
        }
    }
}
