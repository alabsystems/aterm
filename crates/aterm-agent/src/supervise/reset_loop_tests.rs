// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The limit reset's step in the loop (`reset_loop.rs`;
// `docs/SPEC-claude-accounts-resets-2026-10-02.md` §3, §4.B): every exit of
// it, the real `Session` over the scripted server's reset world
// ([`ResetWorld`]) and a scripted host ([`ResetHost`]).
//
// NOTHING HERE HAS BEEN SEEN. Every screen the world shows is COMPOSED from
// the 2.1.287 bundle's texts (`aterm_phase::reset::fixtures`, each saying so
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
                          2 resets left · use by Oct 5 at 3pm";

/// A reset spent, as a host reads it: the vendor queued its own
/// continuation.
fn spent() -> ResetOutcome {
    ResetOutcome::Spent {
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
/// said (`RESET … used (1 left)`); the ledger holds the row, the yes and the
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
                guard_of("   ❯ Yes, use my reset")
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
        "take left=2 day=Oct 5 day_start=Some({})",
        placed("Oct 5 at 12am")
    );
    assert_eq!(host.said("take"), [ask.clone(), ask]);
    assert_eq!(
        host.said("told"),
        [format!(
            "told entered=true kept=- offer=Some((2, Some({}))) last=false",
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
        run.lines.iter().any(
            |l| l.starts_with("RESET seq=") && l.ends_with("rule=limit-reset@v1 used (1 left)")
        ),
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
            format!("approved|Yes, use my reset|{RESET_HEAD}"),
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

    // No host: `aterm drive watch` with the key in its table.
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

/// A host that cannot say yet (`Again`: the wall's own row is not written,
/// its lock is held) is asked again a moment later — and the SIXTH `Again`
/// in a row is a pass: the wait row. NEGATIVE CONTROL: two `Again`s and then
/// the go take the row.
#[test]
fn a_host_that_cannot_say_yet_is_asked_five_times_more_and_then_passed_over() {
    let host = ResetHost::reading(spent());
    host.agains.store(2, Ordering::SeqCst);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "again-2",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1, "{:#?}", run.journal);
    // Two that could not say, the go, and the ask before the Enter.
    assert_eq!(host.said("take").len(), 4);

    let host = ResetHost::reading(spent());
    host.agains.store(usize::MAX, Ordering::SeqCst);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "again-6",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(host.said("take").len(), 6, "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).chosen, [RESET_WAIT]);
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("SKIPPED") && l.ends_with("host=again")),
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
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
        Some(format!("key if-human=0 if={} esc", guard_of("   Use your reset?")).as_str()),
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
            "declined|Use your reset?|inexact-early".to_string(),
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

/// PAST THE VENDOR'S GUARD: no key reaches the confirmation before it has
/// stood the box settle in the loop's sight — a server that refuses, and
/// re-arms on, every key inside its guard sees NONE refused, and the yes is
/// taken by the one Enter. NEGATIVE CONTROL: the same loop with no settle
/// writes its Enter into the guard, where it is refused and nothing is
/// spent.
#[test]
fn no_key_reaches_the_confirmation_before_it_has_stood_the_settle() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).guard = Duration::from_millis(30);
    reset_watch(
        "settle",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| {
            s.set_box_settle(Duration::from_millis(60));
        },
    );
    assert_eq!(reset_world(&mut m).refused, Vec::<String>::new());
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    // The control: a guard that outlasts the run, and no settle.
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).guard = Duration::from_secs(600);
    reset_world(&mut m).exits_when = Some(ResetExit::Refused);
    reset_watch(
        "no-settle",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_world(&mut m).refused.first().map(String::as_str),
        Some("enter")
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
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
            "told entered=false kept=focus offer=Some((2, Some({}))) last=false",
            placed("Oct 5 at 3pm")
        )]
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
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

/// THE ROW'S OWN ANSWER: the check ends with a line and NO confirmation (no
/// reset to use, the limit lifted already). The row's keys are all that is
/// written; the host reads the line — a failure — and the wall's own
/// continuation goes once, by the limit-resume rule. NEGATIVE CONTROL: the
/// whole path, where the same check opens the confirmation.
#[test]
fn a_rows_own_answer_is_a_failure_and_no_key_follows_it() {
    let host = ResetHost::reading(ResetOutcome::Failed { login: false });
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).opens = ResetOpens::Answer;
    let run = reset_watch(
        "answer",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["down@dialog", "enter@dialog"]);
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=- offer=None last=false"]
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("failed (answer)")),
        "{:#?}",
        run.journal
    );
    assert_eq!(
        run.rows().last().map(String::as_str),
        Some("waiting|limit reset|ended: failed")
    );
    assert_eq!(run.typed().len(), 1, "{:#?}", run.ledger);
    assert!(run.typed()[0].contains("usage-resume@v1"));
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// THE ROW OPENS NOTHING: its Enter is written and the dialog stands as it
/// was. Past its bound the reset is kept (`not-opened`), the host told —
/// and the dialog, decided again, is the wait row's: the host answers that
/// the wall has had its attempt. NEGATIVE CONTROL: the whole path, where
/// the same Enter opens the confirmation.
#[test]
fn a_row_that_opens_nothing_is_kept_and_the_wait_row_is_pressed() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).opens = ResetOpens::Nothing;
    let run = reset_watch(
        "nothing",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.open = Duration::from_millis(60))),
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "down@dialog", "enter@dialog"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).chosen, [RESET_GRANT, RESET_WAIT]);
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=not-opened offer=None last=false"]
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("kept (not-opened)")),
        "{:#?}",
        run.journal
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("SKIPPED") && l.ends_with("host=attempted")),
        "{:#?}",
        run.journal
    );
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// THE ROW'S PRESS NEVER LANDS: the cursor does not move onto the grant row
/// however often its arrow is written. The press is tried again on the press
/// back-off — the host answering its own claim with the go again — and
/// where any other box would be badged, the reset is passed over: the host
/// is told it was kept (`not-opened`), NO badge is raised, and the dialog
/// gets its wait row. NEGATIVE CONTROL: the whole path, where the arrow
/// lands and the row is entered.
#[test]
fn a_press_that_never_lands_is_passed_over_and_never_badged() {
    // At full power and under the safe rules alike: the reset's press is
    // no permission, and no level hands it to a person.
    for level in ["all", "safe"] {
        let host = ResetHost::reading(ResetOutcome::Kept);
        // The cursor on the wait row: the grant row is one `up` away, and
        // the wait row needs no arrow.
        let mut m = reset_mock(vec![], &RESET_ROWS, 2, &host);
        reset_world(&mut m).arrow_drops = usize::MAX;
        let mut opts = reset_opts(&host, true, 120);
        opts.policy.set("approve", level).expect("a level");
        let run = reset_watch("never-lands", &mut m, opts, &fresh, |s| {
            s.set_press_badge_after(Duration::from_millis(40));
        });
        let keys = reset_keys(&m);
        assert!(keys.len() > 2, "{level}: {keys:?}");
        assert!(
            keys[..keys.len() - 1].iter().all(|k| k == "up@dialog"),
            "{level}: {keys:?}"
        );
        assert_eq!(keys.last().map(String::as_str), Some("enter@dialog"));
        assert_eq!(reset_world(&mut m).chosen, [RESET_WAIT]);
        assert_eq!(
            host.said("told"),
            ["told entered=false kept=not-opened offer=None last=true"]
        );
        assert!(host.said("take").len() > 1, "its own claim, asked again");
        // Passed over once for the mount: the host is not asked after it.
        let skipped: Vec<&str> = run
            .of_rule()
            .into_iter()
            .filter(|l| l.starts_with("SKIPPED"))
            .collect();
        assert_eq!(skipped.len(), 1, "{level}: {:#?}", run.journal);
        assert!(skipped[0].contains("the press did not land"), "{skipped:?}");
        assert_eq!(m.attention, None, "no badge: {:#?}", m.requests);
        assert_eq!(count(&m, "meta set attention"), 0, "{level}");
        // No step was opened: no row of the rule that a later loop would
        // take up.
        assert!(!run.rows().iter().any(|r| r.starts_with("approved|")));
        assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
    }
}

/// A SECOND MOUNT AT THE SAME WALL: the dialog comes again once the step
/// has ended. The host is asked again — once for the new mount — answers
/// that the wall has had its attempt, and the wait row is pressed; no row
/// is taken a second time. NEGATIVE CONTROL: the first mount of the same
/// run, where the same ask got the go.
#[test]
fn a_second_mount_at_the_same_wall_is_the_wait_rows() {
    use aterm_phase::prompt::fixtures::screen;
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm = screen(aterm_phase::reset::fixtures::CONFIRM_EARLY);
    reset_world(&mut m).exact = false;
    reset_world(&mut m).remounts = 1;
    // The vendor's own wait, once the wait row is taken: nothing to type.
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.continue_policy = false;
    let run = reset_watch("second-mount", &mut m, opts, &fresh, |_| {});
    assert_eq!(
        reset_world(&mut m).chosen,
        [RESET_GRANT, RESET_WAIT],
        "{:#?}",
        run.journal
    );
    assert_eq!(host.said("take").len(), 2, "one ask a mount");
    assert_eq!(host.said("told").len(), 1);
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("SKIPPED") && l.ends_with("host=attempted")),
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    // A reset passed over at one mount is asked about afresh at the next:
    // the host's No, the wait row, the dialog again, and one more ask.
    let host = ResetHost::refusing(ResetNo::NotQualifying);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).remounts = 1;
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.continue_policy = false;
    let run = reset_watch("second-ask", &mut m, opts, &fresh, |_| {});
    assert_eq!(
        reset_world(&mut m).chosen,
        [RESET_WAIT, RESET_WAIT],
        "{:#?}",
        run.journal
    );
    assert_eq!(host.said("take").len(), 2, "one ask a mount");
    // Each press landed: nothing was tried twice.
    assert!(
        !run.journal.iter().any(|l| l.starts_with("WAITING")),
        "{:#?}",
        run.journal
    );
}

/// AN ENTER THE VENDOR DROPS leaves the confirmation standing: a second on,
/// it is judged again and entered again — three Enters at most, and then an
/// Escape (`enter-not-taken`). NEGATIVE CONTROL: one dropped Enter is
/// followed by a second that is taken, and no Escape.
#[test]
fn a_dropped_enter_is_written_again_three_times_at_most_and_then_escaped() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).enter_drops = 1;
    let quick_enter = |s: &mut Session<'_, Mock>| {
        s.set_reset_waits(reset_waits_cut(|w| w.enter = Duration::from_millis(20)))
    };
    reset_watch(
        "drop-1",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        quick_enter,
    );
    assert_eq!(
        reset_keys(&m),
        [
            "down@dialog",
            "enter@dialog",
            "enter@confirm",
            "enter@confirm"
        ]
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).enter_drops = usize::MAX;
    let run = reset_watch(
        "drop-all",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        quick_enter,
    );
    assert_eq!(
        reset_keys(&m),
        [
            "down@dialog",
            "enter@dialog",
            "enter@confirm",
            "enter@confirm",
            "enter@confirm",
            "esc@confirm"
        ],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert!(
        host.said("told")[0].starts_with("told entered=true kept=enter-not-taken "),
        "{:?}",
        host.said("told")
    );
    // One row for the yes, however many Enters.
    assert_eq!(
        run.rows()
            .iter()
            .filter(|r| r.starts_with("approved|Yes, use my reset|"))
            .count(),
        1
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// AN ESCAPE THAT DOES NOT TAKE is tried again on the survey's ladder, the
/// host told ONCE at the second failure — a record, never a badge — and
/// never followed by an Enter. NEGATIVE CONTROL: an Escape that takes is
/// one Escape and no record (the early-use test above).
#[test]
fn an_escape_that_does_not_take_is_tried_again_and_told_once() {
    use aterm_phase::prompt::fixtures::screen;
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm = screen(aterm_phase::reset::fixtures::CONFIRM_EARLY);
    reset_world(&mut m).exact = false;
    reset_world(&mut m).esc_drops = 2;
    let run = reset_watch(
        "esc-ladder",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        [
            "down@dialog",
            "enter@dialog",
            "esc@confirm",
            "esc@confirm",
            "esc@confirm"
        ],
        "{:#?}",
        run.journal
    );
    assert_eq!(
        host.said("inform"),
        ["inform:The limit-reset confirmation did not close\nPress Esc in this tab."]
    );
    assert_eq!(m.attention, None, "a record, never a badge");
    assert_eq!(count(&m, "meta set attention"), 0);
    // One `declined` row, written with the first Escape.
    assert_eq!(
        run.rows()
            .iter()
            .filter(|r| r.starts_with("declined|"))
            .count(),
        1
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A CLAIM WHOSE DIALOG LEFT before its row was taken (a person closed it
/// under the loop's arrows): the host is told the reset was kept
/// (`not-opened`) at the next look, so the wall has had its attempt and its
/// claim holds nobody — and no step is owed. NEGATIVE CONTROL: the whole
/// path, where the dialog stands and the row is taken.
#[test]
fn a_claim_whose_dialog_left_is_told_kept() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).closes_at_key = Some(1);
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.continue_policy = false;
    let run = reset_watch("left", &mut m, opts, &fresh, |_| {});
    assert_eq!(reset_keys(&m), ["down@dialog"], "{:#?}", run.journal);
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=not-opened offer=None last=true"]
    );
    assert!(
        run.of_rule()
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("kept (not-opened)")),
        "{:#?}",
        run.journal
    );
    assert!(!run.rows().iter().any(|r| r.starts_with("approved|")));
    assert_eq!(m.attention, None);
}

/// THE HOST IS ASKED AGAIN FOR THE LINE while it has none — the
/// transcript's row lags the screen — and, its bound run out, ONE LAST TIME
/// (`last`): the step ends on that answer whatever it is. NEGATIVE CONTROL:
/// a host that has the line at the first ask is asked once.
#[test]
fn the_host_is_asked_again_for_the_line_and_then_one_last_time() {
    // Three asks without a line, then the line.
    let host = ResetHost::reading(spent());
    host.pendings.store(3, Ordering::SeqCst);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_watch(
        "line-lags",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    let told = host.said("told");
    assert_eq!(told.len(), 4, "{told:#?}");
    assert!(told.iter().all(|t| t.ends_with(" last=false")), "{told:#?}");
    assert_eq!(host.steps.lock().unwrap().len(), 2, "one claim, one end");

    // Never a line: asked until the bound, then once with `last`.
    let host = ResetHost::reading(ResetOutcome::Unread);
    host.pendings.store(usize::MAX, Ordering::SeqCst);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "no-line",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.line = Duration::from_millis(30))),
    );
    let told = host.said("told");
    assert!(told.len() >= 2, "{told:#?}");
    let (last, before) = told.split_last().expect("told");
    assert!(last.ends_with(" last=true"), "{told:#?}");
    assert!(
        before.iter().all(|t| t.ends_with(" last=false")),
        "{told:#?}"
    );
    assert_eq!(
        run.rows().last().map(String::as_str),
        Some("waiting|limit reset|ended: unread")
    );
}

/// AN ENTER THE SERVER TURNS AWAY (`ERR halted`, `ERR rate`) wrote nothing:
/// the loop parks on the hold, or backs off, reads again and judges again —
/// and the Enter that is written is the one the vendor takes. Nothing is
/// recorded for a key that was not written. NEGATIVE CONTROL: the whole
/// path, one Enter request and one written.
#[test]
fn an_enter_the_server_turns_away_is_judged_again() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm_replies = [err("halted"), err("rate")].into();
    let run = reset_watch(
        "turned-away",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    let at_confirm: Vec<bool> = reset_world(&mut m)
        .probes
        .iter()
        .filter(|p| p.at == ResetAt::Confirm)
        .map(|p| p.written)
        .collect();
    assert_eq!(at_confirm, [false, false, true], "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(host.said("told").len(), 1);
    assert_eq!(
        run.rows()
            .iter()
            .filter(|r| r.starts_with("approved|Yes, use my reset|"))
            .count(),
        1
    );
    assert!(run.journal.iter().any(|l| l.starts_with("HALTED ")));
    assert!(run.journal.iter().any(|l| l.starts_with("REFUSED ")));
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// THE KEY HOLDS AT EVERY `approve` LEVEL AND WITH `limit_wait` OFF: the
/// reset is no permission and no wait. Under `approve = "none"` and under
/// the safe rules, `limit_wait` off, the path is the same three keys.
/// NEGATIVE CONTROL: with the host's `No` the dialog is the owner's there,
/// by the very words a loop with the key off raises.
#[test]
fn the_reset_holds_at_every_approve_level_and_with_limit_wait_off() {
    for level in ["none", "safe"] {
        let limited = |on: bool, host: &Arc<ResetHost>| {
            let mut opts = reset_opts(host, on, 120);
            opts.policy.set("approve", level).expect("a level");
            opts.policy.limit_wait = false;
            opts
        };
        let host = ResetHost::reading(spent());
        let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
        reset_watch("levels", &mut m, limited(true, &host), &fresh, |_| {});
        assert_eq!(
            reset_keys(&m),
            ["down@dialog", "enter@dialog", "enter@confirm"],
            "approve = {level}"
        );
        assert_eq!(reset_world(&mut m).yes_taken, 1);
        assert_eq!(m.attention, None);

        let badge = |on: bool, host: Arc<ResetHost>| {
            let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
            reset_world(&mut m).exits_when = Some(ResetExit::Asked);
            reset_watch("levels-no", &mut m, limited(on, &host), &fresh, |_| {});
            assert_eq!(m.presses(), Vec::<&String>::new(), "approve = {level}");
            let asked: Vec<String> = m
                .requests
                .iter()
                .filter(|r| r.starts_with("meta set attention"))
                .cloned()
                .collect();
            asked
        };
        let off = badge(false, ResetHost::reading(ResetOutcome::Unread));
        let no = badge(true, ResetHost::refusing(ResetNo::NotDefaultLogin));
        assert_eq!(off.len(), 1, "approve = {level}: {off:#?}");
        assert_eq!(no, off, "approve = {level}");

        // A DIALOG THAT DRAWS NO GRANT ROW is read for the reset by nobody:
        // with the key on, every request is the one a loop with the key off
        // sends — the same reads, the same words to the owner — and the host
        // is not asked.
        let plain = |on: bool| {
            let host = ResetHost::reading(ResetOutcome::Unread);
            let rows = [RESET_STOP, RESET_WAIT, RESET_CREDITS];
            let mut m = reset_mock(vec![], &rows, 0, &host);
            reset_world(&mut m).exits_when = Some(ResetExit::Asked);
            reset_watch("levels-plain", &mut m, limited(on, &host), &fresh, |_| {});
            assert!(host.said("").is_empty(), "approve = {level}");
            m.requests.clone()
        };
        let (on, off) = (plain(true), plain(false));
        assert!(
            on.iter().any(|r| r.starts_with("meta set attention")),
            "approve = {level}: {on:#?}"
        );
        assert_eq!(on, off, "approve = {level}");
    }
}

/// THE ROW'S APPROVAL IS NAMED BY ITS OWN LABEL, so the safe rules' cap —
/// the same box approved twice since the last review point is handed over —
/// never counts it against the wait row of the same dialog. Under `approve
/// = "safe"`: the row taken (it opens nothing, and is kept), the wait row
/// pressed and not taken, the wait row pressed again and taken — the second
/// wait press is the wait row's SECOND approval at most, never its third.
/// NEGATIVE CONTROL (seen by mutation): an entry named as the dialog's
/// other approvals are makes that press the third, and the dialog is handed
/// to the owner instead.
#[test]
fn the_rows_approval_is_not_counted_against_the_wait_row() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).opens = ResetOpens::Nothing;
    reset_world(&mut m).row_enter_drops = 1;
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.set("approve", "safe").expect("a level");
    opts.policy.continue_policy = false;
    let run = reset_watch("cap", &mut m, opts, &fresh, |s| {
        s.set_reset_waits(reset_waits_cut(|w| w.open = Duration::from_millis(60)));
    });
    assert_eq!(
        reset_world(&mut m).chosen,
        [RESET_GRANT, RESET_WAIT, RESET_WAIT],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).at, ResetAt::Gone);
    assert_eq!(m.attention, None, "{:#?}", run.journal);
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

/// A PENDING LINE THAT NEVER HOLDS STILL is the look's to read all the
/// same: `Resetting…` repainting for ever, met by a look (the loop came back
/// from an outage), is not taken for output still arriving — the step's
/// bound runs out on it and the host is asked its last word. NEGATIVE
/// CONTROL (seen by mutation): read as a worker at work, the line holds the
/// wait and the host is never told.
#[test]
fn a_pending_line_that_repaints_is_still_the_steps_to_read() {
    let host = ResetHost::reading(ResetOutcome::Unread);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reads = 2;
    reset_world(&mut m).claim_reads = usize::MAX;
    reset_world(&mut m).repaints = true;
    reset_world(&mut m).exits_when = Some(ResetExit::Told);
    let run = reset_watch(
        "repaints",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| {
            s.set_reset_waits(reset_waits_cut(|w| w.claim = Duration::from_millis(60)));
            s.set_reconnect(Duration::from_secs(30));
            s.pause = Duration::from_millis(5);
            s.pause_max = Duration::from_millis(20);
        },
    );
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{:#?}", run.journal);
    assert!(told[0].ends_with(" last=true"), "{told:#?}");
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
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

/// A host whose key is off under a world an earlier loop left its step in:
/// the claim that loop was given still stands, its row taken (`ph`: 1, or 2
/// where the confirmation's Enter was written), and nobody will be told how
/// it ends ([`ResetWorld::untold`]).
fn key_off_after(m: &mut Mock, host: &ResetHost, ph: i64) {
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(ph, Ordering::SeqCst);
    reset_world(m).untold = true;
}

/// What a key-off successor's step left, the same at every confirmation it
/// meets: ONE key, the Escape, no reset spent; the host neither asked nor
/// told nor left a record; the earlier loop's row, the `declined` row and
/// the `waiting` row that ends the step untold; nothing typed, nothing
/// continued, no badge; and no key the model forbids.
fn assert_ended_untold(tag: &str, m: &mut Mock, host: &ResetHost, run: &ResetRun) {
    assert_eq!(reset_keys(m), ["esc@confirm"], "{tag}: {:#?}", run.journal);
    assert_eq!(reset_world(m).yes_taken, 0, "{tag}");
    assert!(host.said("").is_empty(), "{tag}: {:?}", host.said(""));
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|Use your reset?|successor".to_string(),
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
    let began = Instant::now();
    let run = reset_watch(
        "successor",
        &mut m,
        reset_opts(&host, true, 1),
        &taken_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["esc@confirm"], "{:#?}", run.journal);
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "after one grace in its own sight"
    );
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert_eq!(
        host.said("told"),
        [format!(
            "told entered=false kept=successor offer=Some((2, Some({}))) last=false",
            placed("Oct 5 at 3pm")
        )]
    );
    assert!(host.said("take").is_empty(), "a successor asks for no go");
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|Use your reset?|successor".to_string(),
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
    let began = Instant::now();
    let run = reset_watch(
        "key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_ended_untold("key-off", &mut m, &host, &run);
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "after one grace in its own sight"
    );
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
    let began = Instant::now();
    let run = reset_watch(
        "successor-nothing-left",
        &mut m,
        reset_opts(&host, true, 1),
        &taken_row,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["esc@confirm"], "{:#?}", run.journal);
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "after one grace in its own sight"
    );
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
            "declined|Use your reset?|successor".to_string(),
            "waiting|limit reset|ended: kept".to_string(),
        ]
    );
    assert_eq!(m.attention, None);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    // The key off: the same ledger, the same box.
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_the_confirmation(&host, Some(nothing_left_head_off()));
    key_off_after(&mut m, &host, 1);
    let began = Instant::now();
    let run = reset_watch(
        "successor-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_ended_untold("successor-key-off", &mut m, &host, &run);
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "after one grace in its own sight"
    );
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
    let began = Instant::now();
    let run = reset_watch(
        "consent-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        |_| {},
    );
    assert_ended_untold("consent-key-off", &mut m, &host, &run);
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "after one grace in its own sight"
    );

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
        let row = approvals::Row {
            rule_id: "limit-reset@v1",
            outcome: approvals::Outcome::Approved,
            command: "Yes, use my reset",
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
            format!("approved|Yes, use my reset|{RESET_HEAD}"),
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

/// AN OUTAGE AFTER THE ENTER: the reads that follow the confirmation's
/// Enter get no answer. The loop rides the outage out, takes its step up
/// where it stood — `Resetting…`, never keyed — and the host is told the
/// end ONCE. NEGATIVE CONTROL: the whole path, the same world with every
/// read answered, tells it once with no `RECONNECT`.
#[test]
fn an_outage_after_the_enter_still_tells_the_outcome_once() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reads = 2;
    let run = reset_watch(
        "outage",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| {
            s.set_reconnect(Duration::from_secs(30));
            s.pause = Duration::from_millis(5);
            s.pause_max = Duration::from_millis(20);
        },
    );
    assert!(
        run.lines.iter().any(|l| l.starts_with("RECONNECT ")),
        "{:#?}",
        run.lines
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(
        told[0].starts_with("told entered=true kept=- "),
        "{told:#?}"
    );
    assert_eq!(
        run.rows().last().map(String::as_str),
        Some("waiting|limit reset|ended: used")
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    // The worker is at work when the loop comes back (the vendor's own
    // continuation, running): the host is told at that busy read, not at
    // the end of a turn that may run for hours.
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reads = 2;
    reset_world(&mut m).claim_reads = 0;
    reset_world(&mut m).then = vec![busy_screen(), busy_screen(), busy_screen()];
    let run = reset_watch(
        "outage-busy",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| {
            s.set_reconnect(Duration::from_secs(30));
            s.pause = Duration::from_millis(5);
            s.pause_max = Duration::from_millis(20);
        },
    );
    assert_eq!(host.said("told").len(), 1, "{:#?}", run.journal);
    assert!(
        run.lines
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("used (1 left)")),
        "{:#?}",
        run.lines
    );
    assert!(!run.lines.iter().any(|l| l.starts_with("EVENT idle")));

    // ONE busy read, and the turn then ends: the step's end is told AT that
    // read, after the read's own books were kept — and the turn seen running
    // there is the one the continuation was owed for. The line asked for a
    // message; a turn of someone's is already running; nothing is typed
    // after it. NEGATIVE CONTROL (seen by mutation): told at a busy read and
    // not paid there, the continuation is typed once that turn has ended.
    let host = ResetHost::reading(ResetOutcome::Spent {
        asks: true,
        left: Some(1),
        cleared_at: TEST_NOW,
    });
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reads = 2;
    reset_world(&mut m).claim_reads = 0;
    // (The wait that follows the reconnect passes over the first screen of
    // the script: ONE busy screen is read.)
    reset_world(&mut m).then = vec![busy_screen(), busy_screen(), idle_screen()];
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.continue_policy = false;
    let run = reset_watch("outage-one-busy", &mut m, opts, &fresh, riding_out);
    assert_eq!(host.said("told").len(), 1, "{:#?}", run.journal);
    assert!(
        run.lines
            .iter()
            .any(|l| l.starts_with("RESET seq=") && l.ends_with("used (1 left)")),
        "{:#?}",
        run.lines
    );
    assert!(
        !run.lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{:#?}",
        run.lines
    );
    assert_eq!(run.typed(), Vec::<&str>::new(), "{:#?}", run.ledger);
    assert_eq!(count(&m, "send"), 0);
    assert!(
        run.lines.iter().any(|l| l.starts_with("EVENT idle")),
        "the turn ended and its point was reached: {:#?}",
        run.lines
    );
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
            s.pause = Duration::from_millis(5);
            s.pause_max = Duration::from_millis(20);
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
    let mut m = Mock::new(true, vec![idle_screen()]);
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

/// A reconnect window for a run whose server drops an exchange.
fn riding_out(s: &mut Session<'_, Mock>) {
    s.set_reconnect(Duration::from_secs(30));
    s.pause = Duration::from_millis(5);
    s.pause_max = Duration::from_millis(20);
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
    let ctx = ApprovalCtx::new(std::env::temp_dir(), None, 501, None);
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

/// A HALTED SESSION IS ASKED NO CLAIM: `status` says `hold=1`, no key can be
/// written, and a claim taken there would run out under the hold with none.
/// The loop waits the hold out and asks its host only then — the model's
/// `Claim` needs no hold. NEGATIVE CONTROL (seen by mutation): asked under
/// the hold, the projection names the host's `Claim` as forbidden.
#[test]
fn a_halted_session_is_asked_no_claim() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    m.hold = "1";
    m.hold_lifts_after = Some(2);
    let run = reset_watch(
        "halted",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert!(
        run.journal.iter().any(|l| l.starts_with("HALTED ")),
        "{:#?}",
        run.journal
    );
    let claims: Vec<bool> = host
        .steps
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.action == "Claim")
        .map(|s| s.hold)
        .collect();
    assert_eq!(claims, [false], "asked once, and not under the hold");
    assert_eq!(reset_world(&mut m).yes_taken, 1, "{:#?}", run.journal);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A PENDING LINE FIRST SEEN LONG AFTER THE STEP'S LAST KEY HAS ITS WHOLE
/// BOUND FROM THERE. The loop holds its hand for a person who keyed at the
/// confirmation; inside their grace that person presses Enter on the yes
/// themselves, later than the claim's whole bound after the row's Enter.
/// `Resetting…` is then waited on as the vendor's claim it is, and the host
/// is told when the dialog is gone — never at first sight with `last`,
/// which would record a reset being spent as unread. NEGATIVE CONTROL (seen
/// by mutation): bounds counted from the loop's own last key end the step
/// on that first read, the line still up.
#[test]
fn a_pending_line_first_seen_late_has_its_whole_bound() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let world = reset_world(&mut m);
    world.person_at_read = Some(1);
    world.grace = Duration::from_secs(5);
    world.yes_by_person_after = Some(Duration::from_millis(150));
    let run = reset_watch(
        "late-claim",
        &mut m,
        reset_opts(&host, true, 5),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.claim = Duration::from_millis(100))),
    );
    // The loop wrote the row's keys and nothing at the confirmation.
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog"],
        "{:#?}",
        run.journal
    );
    assert_eq!(reset_world(&mut m).yes_taken, 1, "the person's own Enter");
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(
        told[0].starts_with("told entered=true kept=- ") && told[0].ends_with(" last=false"),
        "{told:#?}"
    );
    // The yes was not the loop's: no row of it in the ledger.
    assert!(
        !run.rows().iter().any(|r| r.contains("|Yes, use my reset|")),
        "{:#?}",
        run.rows()
    );
    assert_eq!(
        run.rows().last().map(String::as_str),
        Some("waiting|limit reset|ended: used")
    );
    // Told with no dialog of the step left on the screen: the model's
    // `ReadLine`.
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
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
            format!("approved|Yes, use my reset|{RESET_HEAD}"),
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

/// AN ARROW WHOSE ANSWER NEVER CAME TAKES NO ROW: the press was cut at its
/// focus move, its Enter never sent. No step is opened and no ledger row
/// says a row was taken; after the reconnect the dialog is decided again,
/// the host answers its own claim with the go, and the row is taken and seen
/// through. NEGATIVE CONTROL: a press cut at its ENTER is owed with no go
/// and never entered (`a_row_press_with_no_answer_is_closed_and_never_entered`).
#[test]
fn an_arrow_whose_answer_never_came_takes_no_row() {
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).lose_reply_to = Some((ResetAt::Dialog, "down"));
    let run = reset_watch(
        "lost-arrow",
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
            format!("approved|Yes, use my reset|{RESET_HEAD}"),
            "waiting|limit reset|ended: used".to_string(),
        ]
    );
    assert_eq!(host.said("told").len(), 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
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
            let began = Instant::now();
            let run = reset_watch(&tag, &mut m, reset_opts(&host, on, 1), &old_row, |_| {});
            assert_eq!(reset_keys(&m), ["esc@confirm"], "{tag}: {:#?}", run.journal);
            assert!(
                began.elapsed() >= Duration::from_secs(1),
                "{tag}: after one grace in its own sight"
            );
            assert_eq!(reset_world(&mut m).yes_taken, 0, "{tag}");
            assert!(host.said("").is_empty(), "{tag}: {:?}", host.said(""));
            assert_eq!(
                run.rows(),
                [
                    format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
                    "declined|Use your reset?|successor".to_string(),
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

/// A SCREEN THE STEP CANNOT READ, MET BY A KEY-OFF SUCCESSOR, is waited the
/// open bound and the step ends UNTOLD with NO record left: an owner whose
/// switch is off is left no word about a reset over a screen that may be no
/// dialog at all. No key, nothing typed, no badge. NEGATIVE CONTROL: the
/// same world with the key on leaves the one record and tells its host.
#[test]
fn a_key_off_successor_leaves_no_record_of_a_screen_it_cannot_read() {
    let unread = confirm_frame(&["   One moment", "", "   Refilling your limits"]);
    let at_unread = |host: &Arc<ResetHost>| {
        let mut m = at_the_confirmation(host, None);
        let world = reset_world(&mut m);
        world.at = ResetAt::Resetting;
        world.resetting = unread.clone();
        // It stands far longer than the bound cut below: every read is at
        // least one wait of the server's apart.
        world.claim_reads = 400;
        let frame = world.frame();
        let last = m.screens.len() - 1;
        m.screens[last] = frame;
        m
    };
    let cut = |s: &mut Session<'_, Mock>| {
        s.set_reset_waits(reset_waits_cut(|w| w.open = Duration::from_millis(60)));
    };

    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = at_unread(&host);
    key_off_after(&mut m, &host, 1);
    let run = reset_watch(
        "unread-key-off",
        &mut m,
        reset_opts(&host, false, 1),
        &taken_row,
        cut,
    );
    assert_eq!(reset_keys(&m), Vec::<String>::new(), "{:#?}", run.journal);
    assert_eq!(m.presses(), Vec::<&String>::new());
    assert!(host.said("").is_empty(), "{:?}", host.said(""));
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "waiting|limit reset|ended: untold (key off)".to_string(),
        ]
    );
    assert_eq!(run.typed(), Vec::<&str>::new(), "{:#?}", run.ledger);
    assert_eq!((count(&m, "send"), count(&m, "turn")), (0, 0));
    assert_eq!(m.attention, None);
    assert_eq!(count(&m, "meta set attention"), 0);

    // The control: the key on.
    let host = ResetHost::reading(ResetOutcome::Kept);
    host.shared.claim.store(1, Ordering::SeqCst);
    host.shared.ph.store(1, Ordering::SeqCst);
    let mut m = at_unread(&host);
    let mut opts = reset_opts(&host, true, 1);
    opts.policy.continue_policy = false;
    reset_watch("unread-key-on", &mut m, opts, &taken_row, cut);
    assert_eq!(
        host.said(""),
        [
            "inform:A limit-reset dialog could not be read\nLook at this tab.",
            "told entered=false kept=- offer=None last=false",
        ]
    );
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
    assert_eq!(
        host.said(""),
        ["inform:The limit-reset confirmation did not close\nPress Esc in this tab."]
    );
    assert_eq!(
        run.rows(),
        [
            format!("approved|{RESET_GRANT}|taken: wall until 1790265300"),
            "declined|Use your reset?|successor".to_string(),
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

/// AN ENTER THE SCREEN KEEPS MOVING UNDER IS GIVEN UP: the server skips it
/// (`OK skipped reason=changed`) at every read. It is read and judged again
/// five times, and then the confirmation is Escaped (`inexact-moving`) —
/// never keyed at for the claim's whole life. NEGATIVE CONTROL (seen by
/// mutation): with no bound the seventh Enter, the first the server takes,
/// is written.
#[test]
fn an_enter_the_screen_keeps_moving_under_is_given_up() {
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm_replies =
        std::iter::repeat_with(|| ok("OK skipped reason=changed seq=1\n"))
            .take(6)
            .collect();
    let run = reset_watch(
        "moving",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    let at_confirm: Vec<(String, bool)> = reset_world(&mut m)
        .probes
        .iter()
        .filter(|p| p.at == ResetAt::Confirm)
        .map(|p| (p.key.clone(), p.written))
        .collect();
    let mut want = vec![("enter".to_string(), false); 6];
    want.push(("esc".to_string(), true));
    assert_eq!(at_confirm, want, "{:#?}", run.journal);
    assert_eq!(reset_world(&mut m).yes_taken, 0);
    assert!(
        host.said("told")[0].starts_with("told entered=false kept=inexact-moving "),
        "{:?}",
        host.said("told")
    );
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// A HOST THAT CANNOT SAY AT THE ENTER — its lock held for a moment as the
/// claim is asked about again — is asked again a moment later, and the
/// confirmation is entered on its go: a transient lock does not spend the
/// wall's one attempt. Past five more asks the reset is kept (`claim`).
/// NEGATIVE CONTROL: a host that answers no go at all keeps it at once
/// (`a_claim_that_is_no_longer_the_sessions_keeps_the_reset`).
#[test]
fn a_host_that_cannot_say_at_the_enter_is_asked_again() {
    let host = ResetHost::reading(spent());
    *host.again_at.lock().unwrap() = vec![2];
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "again-enter",
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
    assert_eq!(host.said("take").len(), 3, "the row, again, the go");
    assert_eq!(reset_world(&mut m).yes_taken, 1);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    // It never can: five more asks, and then the reset is kept.
    let host = ResetHost::reading(ResetOutcome::Kept);
    *host.again_at.lock().unwrap() = (2..40).collect();
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_watch(
        "again-enter-never",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"]
    );
    assert_eq!(host.said("take").len(), 7);
    assert!(host.said("told")[0].starts_with("told entered=false kept=claim "));
    assert_eq!(reset_world(&mut m).yes_taken, 0);
}

/// A SCREEN THE READERS CANNOT READ AFTER THE ENTER IS THE VENDOR'S CLAIM
/// UNDER WAY, as far as the loop can tell: it is waited the claim's bound,
/// not the few seconds a row is given to open something — so the step is
/// told ended when the dialog has left, not while a reset is being claimed
/// under a line this build does not know. Past the claim's bound the host
/// is left ONE record that a dialog may stand unread, and told with `last`.
/// NEGATIVE CONTROL (seen by mutation): bounded by the open wait, the step
/// ends with the screen still up.
#[test]
fn an_unread_screen_after_the_enter_is_waited_the_claims_bound() {
    let unread = confirm_frame(&["   One moment", "", "   Refilling your limits"]);
    let host = ResetHost::reading(spent());
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).resetting = unread.clone();
    reset_world(&mut m).claim_reads = 3;
    let run = reset_watch(
        "unread-claim",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.open = Duration::from_millis(1))),
    );
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{:#?}", run.journal);
    assert!(
        told[0].starts_with("told entered=true kept=- ") && told[0].ends_with(" last=false"),
        "{told:#?}"
    );
    assert!(host.said("inform").is_empty());
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());

    // For the whole of the claim's bound: one record, and the last word.
    let host = ResetHost::reading(ResetOutcome::Unread);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).resetting = unread;
    reset_world(&mut m).claim_reads = usize::MAX;
    reset_world(&mut m).exits_when = Some(ResetExit::Told);
    reset_watch(
        "unread-for-ever",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.claim = Duration::from_millis(60))),
    );
    let told = host.said("told");
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(told[0].ends_with(" last=true"), "{told:#?}");
    assert_eq!(
        host.said("inform"),
        ["inform:A limit-reset dialog could not be read\nLook at this tab."]
    );
    assert_eq!(m.attention, None, "a record, never a badge");
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
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
/// NEGATIVE CONTROL: the composed confirmation, exact, is entered (the whole
/// path) — and (seen by mutation) the second box, left to the rules, is
/// answered by keys that are not the step's, and the third, judged the
/// step's only where it follows the dialog, `Checking…` or the
/// confirmation, is the rules': no key, and the badge over a focused `Yes`.
#[test]
fn the_box_the_row_opened_is_the_steps_whatever_it_is_reworded_to() {
    let no_title = confirm_frame(&[
        "   Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am",
        "   2 resets left · use by Oct 5 at 3pm",
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
    // The box's first visible row, and how many reads `Checking…` stands.
    for (tag, frame, first, check_reads) in [
        ("no-title", no_title, 3, 1),
        ("nothing-left", nothing_left, 3, 1),
        ("nothing-left-first-read", nothing_left_head_off(), 0, 0),
    ] {
        let first = frame[first].clone();
        let host = ResetHost::reading(ResetOutcome::Kept);
        let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
        reset_world(&mut m).confirm = frame;
        reset_world(&mut m).exact = false;
        reset_world(&mut m).check_reads = check_reads;
        let run = reset_watch(tag, &mut m, reset_opts(&host, true, 120), &fresh, |_| {});
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
        assert_eq!(m.attention, None, "{tag}");
        assert_eq!(reset_violations(&m, &host), Vec::<String>::new(), "{tag}");
    }
}

/// A CONFIRMATION NO BOX READS UNDER — its title on the screen and its
/// buttons in the vendor's other layout (`y. Yes, use my reset`, READ in the
/// bundle; what turns it on was not traced) — is still the confirmation the
/// loop's own row opened: Escaped on its title row (`inexact-options`), never
/// left standing over the composer with the step ended. NEGATIVE CONTROL
/// (seen by mutation): read as a frame half drawn, the step ends after its
/// open wait with no key written and the dialog still up.
#[test]
fn a_confirmation_no_box_reads_under_is_escaped_on_its_title() {
    let frame = confirm_frame(&[
        "   Use your reset?",
        "",
        "   Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am",
        "   2 resets left · use by Oct 5 at 3pm",
        "",
        "   y. Yes, use my reset",
        "   n. No, keep it",
        "",
        "   Enter y/n: ",
    ]);
    assert!(
        aterm_phase::read(Some("claude"), &frame, None)
            .prompt
            .is_none(),
        "the box reader takes no box here"
    );
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).confirm = frame;
    reset_world(&mut m).exact = false;
    let run = reset_watch("y-n", &mut m, reset_opts(&host, true, 120), &fresh, |s| {
        s.set_reset_waits(reset_waits_cut(|w| w.open = Duration::from_millis(60)))
    });
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"],
        "{:#?}",
        run.journal
    );
    let presses = m.presses();
    assert_eq!(
        presses.last().map(|r| r.as_str()),
        Some(format!("key if-human=0 if={} esc", guard_of("   Use your reset?")).as_str())
    );
    assert_eq!(
        host.said("told"),
        ["told entered=false kept=inexact-options offer=None last=false"]
    );
    assert_eq!(reset_world(&mut m).at, ResetAt::Gone);
    assert_eq!(reset_violations(&m, &host), Vec::<String>::new());
}

/// THE WORKER ALREADY AT WORK WHEN THE STEP ENDS PAYS ITS CONTINUATION: the
/// dialog closed with its answer and a turn — the vendor's own continuation,
/// a person's message — is running on the very read that shows it gone. The
/// host is told there, and the turn seen running is the one the step's end
/// leaves owed: nothing is typed when it ends. NEGATIVE CONTROL: the same
/// answer met at a composer with nobody at work owes its one continuation
/// (`a_rows_own_answer_is_a_failure_and_no_key_follows_it`).
#[test]
fn a_turn_already_running_when_the_step_ends_pays_its_continuation() {
    let host = ResetHost::reading(ResetOutcome::Failed { login: false });
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).opens = ResetOpens::Answer;
    reset_world(&mut m).then = vec![busy_screen(), idle_screen()];
    let mut opts = reset_opts(&host, true, 120);
    opts.policy.continue_policy = false;
    let run = reset_watch("answer-at-work", &mut m, opts, &fresh, |_| {});
    assert_eq!(reset_keys(&m), ["down@dialog", "enter@dialog"]);
    assert_eq!(host.said("told").len(), 1, "{:#?}", run.journal);
    assert!(
        run.lines.iter().any(|l| l.starts_with("EVENT idle")),
        "the turn ended and its point was reached: {:#?}",
        run.lines
    );
    assert!(
        !run.lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{:#?}",
        run.lines
    );
    assert_eq!(run.typed(), Vec::<&str>::new(), "{:#?}", run.ledger);
}
