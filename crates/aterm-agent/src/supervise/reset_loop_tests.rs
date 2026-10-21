// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The limit reset's step in the loop (`reset_loop.rs`;
// `docs/SPEC-claude-accounts-resets-2026-10-02.md` §3, §4.B): every exit of
// it, the real `Session` over the scripted server's reset world
// ([`ResetWorld`]) and a scripted host ([`ResetHost`]).
//
// NOTHING HERE HAS BEEN SEEN. Every screen the world shows is COMPOSED from
// the 2.1.304 bundle's texts (`aterm_phase::reset::fixtures`, each saying so
// on its first line) and every transition of it is READ there, never
// observed: these tests pin what the loop does with the reset program as the
// bundle describes it, and say nothing of a real wall.
//
// Every run is also projected onto `SupervisorLimitReset`
// ([`reset_violations`]): the keys the server wrote and the host's own steps
// must each be enabled where the world stood.

/// A key request with its generation's number masked: the fence is asserted
/// to be there, and which read it names is the script's.
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
/// draws it, joined as the reader joins it.
const RESET_HEAD: &str = "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am \
                          2 weekly resets left · use by Oct 5 at 3pm";

/// A reset spent, as a host reads it: the vendor queued its own
/// continuation.
fn spent() -> ResetOutcome {
    ResetOutcome::Spent {
        kind: aterm_phase::ResetKind::Weekly,
        asks: false,
        left: Some(1),
        cleared_at: TEST_NOW,
    }
}

/// THE WHOLE PATH: with the key on and the host's go, the loop moves the
/// cursor onto the grant row and enters it, waits out `Checking…`, enters
/// the confirmation's yes — once, fenced on the person count, the read's
/// generation and the yes row as drawn — waits out `Resetting…`, and tells
/// its host once. THOSE THREE KEYS AND NOTHING ELSE: no `send`, no `turn`,
/// no arrow at the confirmation. The row is an `APPROVED` line; the spend is
/// said (`RESET … used (1 weekly reset left)`); the ledger holds the row, the yes and the
/// step's end, and no typed act; nothing is asked of the owner.
///
/// NEGATIVE CONTROL: with the host's `No` the dialog gets the wait row by
/// the very requests a loop with the key OFF sends, byte for byte through
/// its last key — and with the key off the host is never asked, where with
/// it on it is asked once a mount.
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
    // The host: asked for the row with its day placed, asked again before
    // the Enter, and told the end once, with the offer the confirmation
    // showed.
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
    // The lines: the row approved, the spend said.
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
    // The books: the row, the yes, the end — and nothing typed.
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

    // THE CONTROL. Key off: the wait row, and the host never asked.
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
    // The wait row's keys as they were before resets existed, spelled out:
    // fenced on the generation and the row, and on nothing else.
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
    // Key on, the host's No: the same requests, and one ask for the mount.
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
    // The same dialog drawn again after a read without it: a new mount (its
    // review key the same), so the host is asked again.
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
    // A host that cannot say yet at a mount the dialog then leaves: the next
    // mount's count of such answers starts afresh, and says so again.
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

/// THE LOOP'S OWN GATES, each shut before the host is asked: `resume_limits`
/// off (a reset is a way of resuming a limit), and no host at all (the drive
/// loop's own run takes no reset). The dialog gets its wait row and the
/// journal names the gate, once. NEGATIVE CONTROL: the whole path above asks
/// the host under the same key with every gate open.
#[test]
fn a_gate_of_the_loops_shut_passes_the_reset_over_before_the_host_is_asked() {
    // `resume_limits` off.
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

    // No host: a loop with no host, the key in its table.
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

/// A SPEND IS SAID, WHATEVER IT LEFT STANDING: after the whole path, a host
/// that read the still-walled line (`ResetOutcome::Walled`, a reset spent
/// at a wall it did not lift) has the step say `RESET … used (no Fable
/// resets left), the wall still stands` — a line, not a journal note — book
/// `ended: used`, and leave the ONE continuation a kept reset leaves owed,
/// typed under `usage-resume@v1`; and a spend whose line named no count
/// (`Spent { left: None }`) is said `used (weekly reset)`, booked the same,
/// with nothing typed (the vendor queued its own continuation). NEGATIVE
/// CONTROL: a reset kept is noted, never said (the early-use test), and the
/// whole path's spend is said with its count.
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

/// THE EARLY-USE CONFIRMATION — the vendor's line that the limit is not
/// reached — is ESCAPED, never entered: one `key if-human=<n> if=<the title
/// row> esc`, the reset kept (`inexact-early`), the ledger's `declined` row
/// on the confirmation's title, and no reset spent. THE BOOKS: the step
/// books no act of the turn-end policy; the ONE continuation it leaves owed
/// is typed after it under `usage-resume@v1`. NEGATIVE CONTROL: the same
/// world with the plain confirmation is entered (the whole path), and there
/// nothing is typed.
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
    // The host was asked once: a confirmation that does not read exactly is
    // kept before the claim is asked about again.
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
    // Kept: journal only, never said.
    assert!(!run.lines.iter().any(|l| l.starts_with("RESET")));
    // The books: one act, the limit-resume rule's, typed after the step.
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

/// THE FOCUS ON NO: a confirmation whose cursor is not on its yes is
/// Escaped — never an arrow to move it, never an Enter on the row it is on.
/// NEGATIVE CONTROL: the same confirmation with the cursor on yes is
/// entered (the whole path).
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

/// A KIND DRAWN BESIDE THE ROW WHILE THE ARROWS MOVE (the resets spec's
/// R2): the full reset, the one reset on offer when the dialog was decided,
/// is reached; as the arrow lands, the dialog draws another kind's row
/// (2.1.304 draws a row per kind as its offers come in, and its Select keeps
/// the focus on the row's value, so the cursor stays on the full reset) —
/// a weekly reset ABOVE it, or a Fable reset UNDER it. Two kinds are then on
/// offer and which to spend is the owner's, so the full reset's row is NOT
/// entered: the press is `Press::Changed`, the dialog is decided again on
/// what is drawn and gets its wait row, the host is told its claim was kept
/// (`not-opened`), and nothing is spent. NEGATIVE CONTROL: the same dialog
/// with no row drawn enters the full reset's row and its confirmation.
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

    // THE CONTROL: no row drawn, the lone full reset's row and its
    // confirmation are entered.
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

/// UNDER THE OWNER'S ORDER (`[harness] limit_reset_kinds`, the resets
/// spec's R2) THE ROW'S PRESS READS THE DIALOG AGAIN BY THAT ORDER, as the
/// row was chosen ([`Session::reset_take`] keeps it). As the arrow lands a
/// weekly reset is drawn above the full reset the dialog was decided on:
///
/// * under `["full", "weekly"]`, the full reset is still the first listed
///   kind on offer, so its row and confirmation are entered at once, the
///   host asked twice (the row, then the Enter) — with no list (the test
///   above) two on offer would leave it unentered;
/// * under `["weekly", "full"]`, with a 5-hour reset on offer beside the
///   full one from the first read, the weekly reset is now the row the rule
///   takes, so the full reset's row is NOT entered and nothing is spent.
///   NEGATIVE CONTROL (seen by mutation): a press that read the dialog again
///   by the lone-offer rule finds no row of its own in the first read (two
///   on offer), checks nothing, and enters the full reset;
/// * and with nothing drawn, a dialog with two on offer from its first read
///   has the listed kind taken and spent under `["full"]` — the policy's
///   context carries the list (`BoxFacts` into `ApprovalCtx`) — where with
///   no list it gets the wait row.
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
    // The full reset first: still the row the rule takes, entered.
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

    // The weekly reset first: the full reset's row is no longer the rule's.
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

    // Two on offer from the first read and nothing drawn: the listed kind
    // is taken and spent, where with no list the dialog is the wait row's.
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

/// THE CLAIM IS ASKED ABOUT AGAIN before the Enter, and anything but the
/// host's go keeps the reset (`claim`): a claim the host no longer answers
/// for, and a claim asked for longer ago than an Enter may still be written
/// on (its ten minutes must outlast the vendor's claim and its line).
/// NEGATIVE CONTROL: the whole path, where the same confirmation is entered
/// on the same host's second go.
#[test]
fn a_claim_that_is_no_longer_the_sessions_keeps_the_reset() {
    // The host's second answer is no go.
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

    // The claim is too old to enter on: the host is not even asked again.
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

/// A PERSON, three ways, and in none of them is a key of the reset's
/// written under their hand:
///
/// 1. a person typed half a second before the dialog was judged: held as
///    any box's answer is, the host never asked, no claim taken;
/// 2. a person keys while the confirmation stands its settle: `HELD`, said
///    once, and the Enter goes only when their grace has passed;
/// 3. a person keys in the round trip between the loop's read and its
///    Enter: the server skips it (`OK skipped reason=person`), nothing is
///    spent, and the Enter is written again only after their grace.
///
/// NEGATIVE CONTROL: the whole path, the same loop with nobody keying, has
/// no `HELD` line and one Enter.
#[test]
fn a_person_holds_every_key_of_the_reset() {
    // 1. Before the row.
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

    // 2. During the settle (the world's grace and the loop's: a second).
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

    // 3. In the round trip before the Enter.
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

/// A PENDING LINE IS WAITED ON, NEVER KEYED — but `Checking for a reset…`
/// past its bound, which gets ONE guarded Escape on its own row
/// (`check-timeout`); `Resetting your limits…` past its bound gets NO key at
/// all: the host is asked one last time and the step ends. NEGATIVE CONTROL:
/// the whole path, where both lines leave inside their bounds and neither is
/// keyed.
#[test]
fn a_pending_line_is_never_keyed_but_checking_past_its_bound() {
    // `Checking…` for the whole bound.
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

    // `Resetting…` for the whole bound: the Enter, and no key after it.
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

/// NO ENTER FOLLOWS AN ESCAPE. The confirmation's cursor is on its no: the
/// loop Escapes — and the Escape does not take, while a person's arrow moves
/// the cursor onto the yes. The confirmation now reads exactly, cursor and
/// all, and is STILL kept: the Escape is written again, never an Enter.
/// NEGATIVE CONTROL: the same confirmation found with its cursor on the yes
/// and no Escape written is entered (the whole path).
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

/// THE LOOP'S STOP, set in every wait of the whole path in turn: no wait
/// and no key reaches the transport after it, and the run ends as a stop —
/// what the step had reached stays in the session ledger for the loop that
/// follows.
#[test]
fn a_stop_in_any_wait_of_the_step_ends_it_with_nothing_written_after() {
    let host = ResetHost::reading(spent());
    let opts = SuperviseOpts {
        max: UNBOUNDED,
        ..reset_opts(&host, true, 120)
    };
    let armed = stop_in_every_wait("reset-step", &opts, None, || {
        // One host for every run of the sweep: its claim begins anew.
        for n in [&host.shared.claim, &host.shared.ph] {
            n.store(0, Ordering::SeqCst);
        }
        host.shared.own.store(false, Ordering::SeqCst);
        reset_mock(vec![], &RESET_ROWS, 0, &host)
    });
    assert!(armed.len() > 6, "{armed:#?}");
}

/// The session ledger's row of a grant row an earlier loop took, as it
/// wrote it.
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
/// not see the row taken — or on `confirm` in its place, a box that does
/// not read exactly.
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

/// A CONFIRMATION OF ANOTHER KIND THAN THE ROW TAKEN IS KEPT (the resets
/// spec's R11): the 5-hour row taken, at a wall drawn by both the session's
/// and the week's names, the box that mounts is the WEEKLY reset's — exact
/// as it reads, its count and day the row's — and it is Escaped (`kind`):
/// no reset spent, the host told it was kept. Every other check would have
/// entered it: only the kind tells it from the row's own. NEGATIVE CONTROL:
/// the same row with its own confirmation (`Use a 5-hour reset?`, `2
/// five-hour resets left`) is entered, the ask naming the row's kind, and
/// the ledger's Enter names that kind's yes.
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

    // The control: the row's own confirmation, entered.
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

    // Another kind's confirmation after the same row: kept.
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
/// the claim that loop was given still stands, its row taken (`ph`: 1, or 2
/// where the confirmation's Enter was written), and nobody will be told how
/// it ends ([`ResetWorld::untold`]).
fn key_off_after(m: &mut Mock, host: &ResetHost, ph: i64) {
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(ph, Ordering::SeqCst);
    reset_world(m).untold = true;
}

/// What the `declined` row of a successor's Escape names on the world's
/// confirmation: its title as drawn (`Use a weekly reset?`), or, where none
/// is drawn, the untitled name — a successor holds no go whose kind would
/// name one.
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
/// meets: ONE key, the Escape, no reset spent; the host neither asked nor
/// told nor left a record; the earlier loop's row, the `declined` row and
/// the `waiting` row that ends the step untold; nothing typed, nothing
/// continued, no badge; and no key the model forbids. The `declined` row
/// names the confirmation's title as drawn, or — on a box whose title is
/// reworded or off the pane, a successor holding no go — the untitled name.
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

/// THE SUCCESSOR: a loop that starts on a ledger whose last row of the rule
/// is the row's `approved` one finds the confirmation up. It is Escaped
/// once it has stood one person's grace in this loop's sight (`successor`),
/// NEVER entered — exact as it reads, the cursor on its yes — and the host
/// is told. NEGATIVE CONTROL: with no such row the confirmation is a
/// PERSON's: no step, the host told nothing, and the ordinary rules decline
/// it by its No row.
///
/// WITH THE KEY OFF the open row is this loop's to end all the same — the
/// key decides whether a row is taken, never whether an open one is ended:
/// the same one Escape after the same grace, and the step ends UNTOLD
/// ([`assert_ended_untold`]). NEGATIVE CONTROL (seen by mutation): with the
/// ledger asked only under the key, the confirmation is the ordinary rules'
/// and gets `down`, `enter` on its No row, with no row of the rule written.
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

    // The control: no row of the rule, and the same screen.
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

    // The KEY OFF, the earlier loop's row, the same screen.
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

/// A SUCCESSOR'S FIRST LOOK MEETS A BOX WITH NOTHING OF THE CONFIRMATION
/// LEFT, its head off the pane, and it is the step's all the same: Escaped
/// once it has stood one person's grace in this loop's sight (`successor`),
/// never answered, no reset spent. NEGATIVE CONTROL (seen by mutation):
/// judged the step's only where it follows the dialog, `Checking…` or the
/// confirmation, it is the rules', which hand a box whose head they cannot
/// read to a person: no key, and the badge.
///
/// WITH THE KEY OFF the same Escape, and the step ends untold
/// ([`assert_ended_untold`]). NEGATIVE CONTROL (seen by mutation): with the
/// ledger asked only under the key, the box is the rules' again — no key,
/// no row of the rule, and the badge raised over a focused `Yes`.
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

    // The key off: the same ledger, the same box.
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

/// The weekly reset's confirmation in Claude Code 2.1.304's SCREEN-READER
/// layout (`sr` @234846790 draws `oir` @234804179 where `ot()` says a
/// screen reader is on): the title and body as ever, then LETTERED answers
/// and a typed one — `y. Yes, use my weekly reset`, `n. No, keep it`,
/// `Enter y/n: `. COMPOSED, never captured. The reader takes no box from it
/// (no option is read), so only its title says it is the confirmation.
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

/// A CONFIRMATION WITH NO BOX THE READER TAKES, ITS TITLE STILL UP, IS THE
/// STEP'S UNTIL ITS ENTER IS WRITTEN (`reset_shows`: an owed step whose Enter
/// was not written reads a `Use a <kind>?` row as its confirmation,
/// `inexact-options`): a successor facing 2.1.304's screen-reader layout
/// ([`screen_reader_confirmation`]) Escapes it once it has stood one
/// person's grace, under the guard of the TITLE row (the vendor's Escape
/// keeps the reset there too), and never types `y` or presses Enter: no
/// reset spent. NEGATIVE CONTROL: once the earlier loop's Enter is on the
/// ledger the title stands over the vendor's claim, and nothing is keyed on
/// it at all, for five times the grace. (Seen by mutation: with the title
/// test gone from `reset_shows`, the first frame is read as nothing and no
/// key goes; with it asked after the Enter too, the claim's title is
/// Escaped.)
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

    // The control: the earlier loop's Enter on the ledger, the same frame.
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
    // The claim's screen stands as long as the watch: five times the grace
    // after which the same frame was Escaped above.
    let mut opts = reset_opts(&host, true, 1);
    opts.max = Duration::from_secs(5);
    let run = reset_watch("screen-reader-entered", &mut m, opts, &entered_row, |_| {});
    assert_eq!(reset_keys(&m), Vec::<String>::new(), "{:#?}", run.journal);
    assert_eq!(m.presses(), Vec::<&String>::new());
    assert!(reset_world(&mut m).reads > 1, "the frame was read again");
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(m.attention, None);
}

/// THE YES FULL POWER WOULD TAKE: a key-off successor's first look meets the
/// confirmation reworded whole WITH ITS HEAD ON THE PANE, its yes worded as
/// a limit's consent to go on (`Continue with …`) — the one box of the
/// row's that the approval rules answer yes to (`allow-once@v1`). It is the
/// step's: Escaped after one person's grace, never entered, no reset spent,
/// and the step ends untold ([`assert_ended_untold`]).
///
/// NEGATIVE CONTROLS. (a) The same loop, key and box on a ledger with NO
/// open row: no step, and the rules take the yes exactly as they did before
/// — `enter` on it, under `allow-once@v1`, the host silent and no row of the
/// reset's rule. (b) Seen by mutation: with the ledger asked only under the
/// key, the run WITH the open row is (a)'s — the yes taken, a reset spent
/// against the owner's switch.
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

    // (a) No open row: today's answer, to the byte of its one key — on no
    // ledger at all, on one whose step of the rule was ENDED (each of the
    // three ways a `waiting` row ends one), and on one whose only open row
    // is another session's.
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
        // Not one row of the reset's rule was added to what the seed left.
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
        // The keys the loop sent and what it added to the ledger (its
        // times apart) are the same on every one of them.
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

/// `RESETTING…` MET BY A KEY-OFF SUCCESSOR — the earlier loop wrote the
/// confirmation's Enter and died — is never keyed, and when the dialog has
/// left the step ends UNTOLD: the `waiting` row, the host neither asked nor
/// told, and NOTHING OWED — the composer is left as it is. NEGATIVE CONTROL:
/// the same world with the key on tells the host once (`entered=true`) and
/// types the one continuation its answer leaves owed.
#[test]
fn a_key_off_successor_waits_out_resetting_and_owes_nothing() {
    let entered_row = |ledger: &Path| {
        taken_row(ledger);
        // The Enter as an OLDER aterm's ledger names it (2.1.287–2.1.303:
        // `Yes, use my reset`, no kind): read as written all the same
        // (`approvals::reset_owed`), so the successor never keys.
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

    // The control: the key on.
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

/// THE ROW'S PRESS GOT NO ANSWER: its Enter was written and taken, and the
/// reply never came. What the row opened is owed its end — the ledger says
/// so — and is NEVER entered, although it reads exactly: Escaped after one
/// grace in the loop's sight, as a successor's. NEGATIVE CONTROL: the whole
/// path, where the same press is answered and the same confirmation
/// entered.
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

/// A DAY AND HOUR OF THE VENDOR'S IS PLACED in the wall notice's zone where
/// the loop holds one, else the local zone, in the year nearest now — and
/// nothing that is not a dated clock time places at all. NEGATIVE CONTROL:
/// the same words with no notice held land in the local zone, seven hours
/// off.
#[test]
fn a_deadline_is_placed_in_the_wall_notices_zone_where_the_loop_holds_one() {
    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    // No notice held: the local zone (the tests' is PDT).
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
    // A DAY DRAWN WITH ITS YEAR IS THAT YEAR'S, however near the same day
    // of another year is: a grant that ends a year and a day out is not one
    // that ends tomorrow. (The clock stands at Sep 17, 2026.)
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
    // Not a dated clock time: a bare clock, a span, a day alone, junk.
    for not in ["3pm", "in 3h", "Oct 5", "soon"] {
        assert_eq!(s.reset_place(not), None, "{not}");
    }
    // The local zone's offset ON THE DAY, where the machine can say: a day
    // the other side of a daylight-saving change is an hour off the offset
    // that holds now. NEGATIVE CONTROL: the lines above, where the machine
    // says nothing and the offset of now places every day.
    s.set_local_lookup(|unix| (unix > TEST_NOW + 10 * 86_400).then_some(PDT - 3600));
    assert_eq!(s.reset_place("Oct 5 at 3pm"), Some(local + 3600));
    assert_eq!(s.reset_place("Sep 18 at 3pm"), Some(tomorrow));
    s.set_local_lookup(|_| None);
    // The wall's notice named its zone: the deadline is placed there.
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
/// a confirmation as a server's flag payload might redraw it. COMPOSED, as
/// every screen here.
fn confirm_frame(body: &[&str]) -> Vec<String> {
    use aterm_phase::prompt::fixtures::screen;
    let mut rows: Vec<String> = screen(aterm_phase::reset::fixtures::CONFIRM)[..3].to_vec();
    rows.extend(body.iter().map(|row| (*row).to_string()));
    rows
}

/// The composed confirmation with NOTHING of it left — its body and both
/// labels reworded — and its head scrolled off the pane's top: a box of no
/// known kind whose head no rule can read, which the approval policy hands
/// to a person at every level. COMPOSED, as every screen here.
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

/// The composed confirmation with NOTHING of it left and its head ON the
/// pane, its yes reworded to a limit's consent to go on: a box of no known
/// kind, unnumbered, that full power answers yes to (`allow-once@v1`, the
/// consent arm of the rules for a dialog of no kind) — and so the one
/// rewording of the confirmation whose yes the approval policy would take.
/// COMPOSED, as every screen here.
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

/// THE ENTER IS JUDGED ON THE WALL AS THE HOST READS IT AT THE ENTER, not as
/// it stood when the row was taken: a wall that stood more than a day at the
/// row and no longer does when the claim is asked about again is held to
/// the deadline's rule — and a deadline eighteen days off keeps the reset
/// (`not-qualifying`). NEGATIVE CONTROL: the whole path, the same
/// confirmation under a go that is long both times, is entered.
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

/// A PERSON WHO KEYS AFTER THE DIALOG WAS JUDGED — after its read and its
/// `status`, while the host is asked for the claim — gets no key of the
/// row's under their hand: every key of the grant row carries the judged
/// read's person count, the server skips the arrow (`OK skipped
/// reason=person`), and the row is taken only once their grace has passed,
/// on the claim the session still holds. NEGATIVE CONTROL: the whole path,
/// where nobody keys and the first arrow is written.
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

/// AN ENTER WHOSE ANSWER NEVER CAME IS ON THE BOOKS: cut mid-exchange, the
/// key may have been written — here it was, and the yes taken — so the
/// ledger's `approved` row on the vendor's yes and the journal's `confirmed`
/// line are written BEFORE the outage is ridden out, and the host is told
/// the Enter was written. NEGATIVE CONTROL: the whole path writes the same
/// row for an Enter that was answered — and (seen by mutation) a loop that
/// only counts the lost Enter leaves a spent reset with no yes on its books.
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

/// A ROW OLDER THAN A STEP CAN BE IS ENDED UNTOLD, AT EITHER KEY: a loop that
/// starts finds a grant row's `approved` row from long ago with no end (no
/// loop ran since). The host's claim for it has lapsed and a line in the
/// transcript may be a later dialog's, so the host is asked and told
/// nothing and no continuation is owed — but a box that stands is Escaped as
/// any successor's is, never handed to a rule that could say yes to it: the
/// plain confirmation, and the one reworded whole whose yes full power
/// would take (`nothing_left_consent`). The ledger's `waiting` row says
/// `ended: stale`. NEGATIVE CONTROLS: the same row written now, with the key
/// on, is a step whose end the host is told
/// (`a_successor_escapes_the_confirmation_and_never_enters_it`); and (seen
/// failing before the stale row owed a step) the reworded box's yes is
/// taken by `allow-once@v1`: a reset spent.
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
            // The earlier loop's row was taken; its claim has lapsed, and
            // nobody is told how the step ends.
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

/// `CHECKING…` MET BY A KEY-OFF SUCCESSOR and standing past its bound gets
/// the step's ONE guarded Escape on its own row (`check-timeout`), as with
/// the key on, and the step ends UNTOLD: the host neither asked nor told,
/// nothing typed, no badge. NEGATIVE CONTROL: the key-on run of the same
/// bound (`a_pending_line_is_never_keyed_but_checking_past_its_bound`) tells
/// its host `kept=check-timeout`.
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

/// AN ESCAPE THAT DOES NOT TAKE, WITH THE KEY OFF: tried again on the same
/// ladder, the ONE record at the second failure — the only word an owner
/// has of a session held behind a dialog, a record and never a badge — and
/// the step still ends untold once it closes. Never an Enter.
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

/// AN ATTENDED LOOP (`supervise`) TAKES NO STEP UP, at either key: it never
/// makes the step's look, so a step it owed could not end. On a ledger with
/// a fresh open row its every key is the one it sends on a ledger with
/// none, it holds no step when it returns, and the ledger's rows of the
/// rule are as it found them — the row stays for the next unattended loop.
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

/// THE BOX THE ROW OPENED IS THE STEP'S, WHATEVER IT IS REWORDED TO. Every
/// text of the reset is the vendor's server's to change:
///
/// 1. a confirmation with its title off the pane and its yes reworded still
///    keeps its no and its body, and reads as a confirmation with no title;
/// 2. one with NOTHING of the confirmation left — title, body and both
///    labels reworded — is a box of no known kind, with unnumbered options,
///    that directly follows `Checking…` in this loop's own sight;
/// 3. such a box met on the step's FIRST read — `Checking…` came and went
///    between two reads — with its head off the pane, which the approval
///    policy hands to a person at every level.
///
/// Each is ESCAPED under the guard of its first visible row (`inexact-title`)
/// and never handed to a rule of the approval policy: no reset is spent.
/// With no title drawn, the ledger's `declined` row names the title of the
/// row TAKEN's kind (`Use a weekly reset?` after the weekly row, `Use a
/// 5-hour reset?` after the 5-hour one), never the kind-less name a
/// successor with no go holds.
/// NEGATIVE CONTROL: the composed confirmation, exact, is entered (the whole
/// path) — and (seen by mutation) the second box, left to the rules, is
/// answered by keys that are not the step's, and the third, judged the
/// step's only where it follows the dialog, `Checking…` or the
/// confirmation, is the rules': no key, and the badge over a focused `Yes`.
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
    // The box's first visible row, and how many reads `Checking…` stands;
    // the row taken, the wall's limits and the title its kind names.
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

/// THE STEP NEVER ESCAPES AN MCP SERVER DIALOG: Claude Code saves its Esc
/// as a refusal for good (2.1.301, MEASURED; the several-server dialog's is
/// `Esc to reject all`), so the box is never read as a reworded
/// confirmation, on the step's first read or after `Checking…`: not as
/// drawn, not retitled (no MCP kind), and not with the several-server
/// dialog's head cut — its rule alone (the title still drawn) or its rule,
/// title and subtitle — where the box reads with its head off the screen.
/// Every such box still offers an answer read as kept, so it is the
/// approval policy's, which hands it to a person: no Esc, no reset spent.
/// Nor is a frame half drawn: the several-server dialog drawn only down to
/// its server rows, its rule cut (no `Enable selected`, no footer: its head
/// off the screen and no Esc read), read as a frame half drawn until the
/// step's open bound (cut short here) runs out, never keyed; and the
/// retitled dialog read mid-move, a cursor on a server row and on `Enable
/// selected` (the reader withholds every role of options with two
/// cursors). Nor any frame MEASURED on 2.1.302 in a narrow pane
/// ([`aterm_phase::prompt::fixtures::MCP_SERVER_NARROW`]): its labels
/// wrapped, its head cut down to the cursor's wrapped `Continue without using
/// this MCP` over `server` (40x6), which read a row at a time offered no kept
/// answer and was Escaped as `inexact-title`; the cursor's label over the
/// unmarked option under it, which a row's fill alone reads as its rest
/// (60x11, 35x11, and DERIVED at 60x10 and 35x10 with the first option off
/// the screen, [`aterm_phase::prompt::fixtures::MCP_SERVER_DERIVED`], where,
/// joined into one option no kept answer names, it was Escaped the same
/// way); and the frame that shows no
/// box (30x5: the option's last row and the footer), read as unread until
/// the open bound, never keyed, with no box for the policy. NEGATIVE
/// CONTROLS (seen by
/// mutation): with the kept-answer test gone from `reset_shows`, the
/// retitled dialogs are Escaped as `inexact-title`; with `Enable selected`
/// read as no kept answer, so are the several-server dialog's retitled and
/// cut frames; with the kept test read off the roles alone, the mid-move
/// frame; and with a box whose head and Esc are unread taken, the half-drawn
/// frame (read as another box instead, it is never said to the host).
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
    // Drawn top down as far as its last server row, the rows under it not
    // yet drawn.
    let half: Vec<String> = two
        .iter()
        .enumerate()
        .map(|(i, r)| if i < submit { r.clone() } else { String::new() })
        .collect();
    // The cursor drawn on `Enable selected` before it is erased from the
    // first server row.
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
    // Each frame, and whether the step's open bound is run to its end on it.
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
        // A frame with no box is no box the policy can hand on: the step
        // ends once it has told the host.
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
            // A frame half drawn, or one with no box, is said to the host
            // once its bound runs out.
            assert_eq!(host.said("inform").len(), usize::from(unread), "{tag}");
            // The approval policy's: handed to a person — when there is a
            // box to hand.
            assert_eq!(m.attention.is_some(), boxed, "{tag}: {:#?}", run.journal);
        }
    }
}

/// THE STEP NEVER ESCAPES AN ANSWER KEPT FOR GOOD BY ITS LABEL under a rest
/// that opens with no capital: the safety check's box, SYNTHETIC
/// ([`crate::supervise::policy::approval::tests::always_under_a_lowercase_rest`]),
/// of no kind the reader knows, unnumbered, `❯ Continue with tools, always
/// on` over `Keep waiting for` over `now`, met where the confirmation
/// should be, on the step's first read and after `Checking…`. Its `always`
/// still reads Persist where the rest blanks the block's other roles, so it
/// is no reworded confirmation: the approval policy's, which hands it to a
/// person — no Esc, no reset spent. With the rule reading it Other, the step
/// Escaped it as `inexact-title`. NEGATIVE CONTROL: without `always`, the
/// same box is the reworded confirmation the step Escapes.
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
                // The approval policy's: handed to a person.
                assert!(m.attention.is_some(), "{tag}: {:#?}", run.journal);
            } else {
                // NEGATIVE CONTROL.
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
