// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE OWNER'S STANDING GO-AHEAD THROUGH THE LOOP (owner, 2026-10-29: "how dare you block on me
// for go ahead!"): the person's hour holds a park, and nothing the parked agent does ends it
// early or keeps it past its end.

use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
use crate::supervise::policy::turn_end::GO_AHEAD_HELD;

/// Recovery uses signed wall-clock stamps, including hostile extremes, without renewing an
/// expired hour. A future stamp retains the existing conservative zero-age interpretation.
#[test]
fn hour_recovery_signed_stamps_preserve_only_the_remaining_hour() {
    use crate::supervise::approvals::{Outcome, Row};
    use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
    use crate::supervise::policy::turn_end::RULE_GO_AHEAD_HOUR;
    let dir = crate::supervise::ScratchDir::new("go-ahead-hour", "signed-recovery");
    let path = dir.join("books.jsonl");
    let now_ms = 1_790_000_000_000_i64;
    let origin = Instant::now();
    for (from, remaining) in [
        (now_ms - 20 * 60_000, 40 * 60),
        (now_ms - 60 * 60_000, 0),
        (now_ms - 120 * 60_000, 0),
        (now_ms + 1, 60 * 60),
        (i64::MAX, 60 * 60),
        (i64::MIN, 0),
    ] {
        let body = Row {
            rule_id: RULE_GO_AHEAD_HOUR,
            outcome: Outcome::Waiting,
            command: &format!("from={from}"),
            reason: "a later turn end",
            box_seq: 1,
        }
        .to_json(now_ms, Some("@s-9"));
        let mut ctl = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut ctl, Some("@s-9".to_string()));
        s.set_fx(Fx::over(ClockSource::Virtual(Arc::new(VirtualClock::new(
            origin, now_ms,
        )))));
        assert!(
            s.seed_from_ledger_text(&path, &body, now_ms, false)
                .is_none()
        );
        assert_eq!(
            s.turn_end.go_ahead_held(origin),
            remaining > 0,
            "from={from}"
        );
        if remaining > 0 {
            assert!(
                s.turn_end
                    .go_ahead_held(origin + Duration::from_secs(remaining - 1))
            );
        }
        assert!(
            !s.turn_end
                .go_ahead_held(origin + Duration::from_secs(remaining)),
            "from={from}"
        );
        drop(s);
        assert!(ctl.requests.is_empty(), "seeding writes no input");
    }
}

/// An age that cannot be represented by Instant is already expired, rather than a new hour.
#[test]
fn hour_recovery_monotonic_underflow_never_renews_an_expired_hour() {
    let now = Instant::now();
    assert!(now.checked_sub(Duration::MAX).is_none());
    let mut state = TurnEndState::default();
    state.seed_go_ahead_hour(Duration::MAX, now);
    assert!(!state.go_ahead_held(now));
}

/// The startup cut must leave the reader's maximum valid start for each session, even when
/// append order differs from clock order. A restart still observes the remaining later-turn hour.
#[test]
fn hour_recovery_after_a_cut_keeps_each_sessions_maximum_valid_start() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
    use crate::supervise::policy::turn_end::RULE_GO_AHEAD_HOUR;
    let dir = crate::supervise::ScratchDir::new("go-ahead-hour", "cut-recovery");
    let path = dir.join("books.jsonl");
    let now_ms = 1_790_000_000_000_i64;
    let hour = |sid, command: &str| {
        Row {
            rule_id: RULE_GO_AHEAD_HOUR,
            outcome: Outcome::Waiting,
            command,
            reason: "a later turn end",
            box_seq: 1,
        }
        .to_json(now_ms, Some(sid))
    };
    let start = now_ms - 20 * 60_000;
    let other = now_ms - 5 * 60_000;
    let mut rows = vec![
        hour("@s-9", &format!("from={start}")),
        hour("@s-9", &format!("from={}", now_ms - 40 * 60_000)),
        hour("@s-8", &format!("from={other}")),
        hour("@s-9", "from=not-a-time"),
    ];
    let words = "keep going ".repeat(110);
    rows.extend((0..1100).map(|seq| {
        Row {
            rule_id: "continue@v1",
            outcome: Outcome::Typed,
            command: &words,
            reason: "ordinary later work",
            box_seq: seq,
        }
        .to_json(now_ms, Some("@s-9"))
    }));
    let body = rows.join("\n");
    assert!(body.len() > 1 << 20, "the cut must run");
    assert_eq!(
        approvals::go_ahead_hour_in(&body, Some("@s-9")),
        Some(start)
    );
    std::fs::write(&path, &body).expect("owned ledger");
    approvals::bound(&path);
    let cut = std::fs::read_to_string(&path).expect("cut ledger");
    assert!(cut.len() < body.len(), "the ledger was actually cut");
    let origin = Instant::now();
    let mut ctl = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut ctl, Some("@s-9".to_string()));
    s.set_fx(Fx::over(ClockSource::Virtual(Arc::new(VirtualClock::new(
        origin, now_ms,
    )))));
    assert!(
        s.seed_from_ledger_text(&path, &cut, now_ms, false)
            .is_none()
    );
    assert!(
        s.turn_end.go_ahead_held(origin),
        "the cut must preserve the live hour"
    );
    assert!(
        !s.turn_end
            .go_ahead_held(origin + Duration::from_secs(40 * 60))
    );
    assert_eq!(approvals::go_ahead_hour_in(&cut, Some("@s-9")), Some(start));
    assert_eq!(approvals::go_ahead_hour_in(&cut, Some("@s-8")), Some(other));
}

/// The agent's park in the turn `prompt` opened, having worked `worked` (Claude Code's done row).
fn parked(prompt: &str, worked: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("❯ {prompt}"),
        "",
        "⏺ Committed on my branch.",
        "  NEEDS YOU: say \"rerun the checks\" so I can push my branch to main.",
        "",
        &format!("✻ Worked for {worked} · done 4:24 PM"),
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// THE HOUR'S END DECIDES A PARKED POINT AGAIN (2026-10-30 review 8): the person's key comes in
/// the agent's turn, which ends on a park; Cruise's continuation answers it twice, the agent
/// parks the same way each time, and the same reply's hold forms within the hour. The agent
/// then sits idle on that screen, and the loop types the go-ahead once the hour is over, never
/// within it.
#[test]
fn a_park_the_hour_held_gets_the_go_ahead_at_its_end_on_an_unchanged_screen() {
    let mut m = Mock::new(vec![
        busy_screen(),
        parked("status?", "2m 0s"),
        busy_screen(),
        parked("keep going", "5s"),
        busy_screen(),
        parked("keep going", "5s"),
    ]);
    m.autopilot = Some("cruise");
    m.turn_gates = vec![1, 3];
    m.keyed_at_read = Some(0);
    m.stall_sleep = Some(Duration::from_secs(20));
    let (dir, journal) = journal_file("go-ahead-hour");
    let opts = SuperviseOpts {
        policy: owners_policy(),
        journal: Some(journal.clone()),
        ..auto(3 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    let noted: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    let key = m.keyed_at.expect("the person's key");
    let typed: Vec<(&String, Instant)> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .zip(m.turn_times.iter().copied())
        .collect();
    let go_ahead = typed
        .iter()
        .position(|(r, _)| r.contains(GO_AHEAD_ANSWER))
        .unwrap_or_else(|| panic!("the go-ahead: {lines:#?}\n{noted:#?}"));
    assert!(
        go_ahead >= 2,
        "the continuation answers it first: {typed:#?}"
    );
    assert!(
        typed[go_ahead].1 >= key + GO_AHEAD_HELD,
        "never within the hour: {typed:#?}"
    );
}

/// ANOTHER HARNESS PROCESS'S HAND IS NO PERSON'S KEY (2026-10-30 review 8): `aterm harness
/// upgrade` holds the tab by a lease of its own (`aterm-harness@<its pid>`) while the agent sits
/// on its park, the person's last key two hours back. Once the lease goes the park gets the
/// go-ahead at once: the lease neither started the person's hour nor came back as their key.
#[test]
fn another_harness_processs_hand_holds_no_go_ahead_hour() {
    let mut m = Mock::new(vec![
        busy_screen(),
        parked("status?", "2m 0s"),
        parked("status?", "2m 0s"),
        parked("status?", "2m 0s"),
        parked("status?", "2m 0s"),
    ]);
    m.autopilot = Some("cruise");
    m.human_ms = Some(2 * 3600 * 1000);
    m.hand_from.insert(0, "lease:aterm-harness@1");
    m.hand_from.insert(4, "-");
    m.stall_sleep = Some(Duration::from_secs(20));
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(3 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    let start = m.read_times.first().expect("a read").1;
    let typed: Vec<(&String, Instant)> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .zip(m.turn_times.iter().copied())
        .collect();
    let (_, at) = typed
        .iter()
        .find(|(r, _)| r.contains(GO_AHEAD_ANSWER))
        .unwrap_or_else(|| panic!("the go-ahead: {lines:#?}\n{typed:#?}"));
    assert!(
        *at < start + Duration::from_secs(30 * 60),
        "at once, not an hour after the lease: {typed:#?}"
    );
}

/// A DRIVER'S HAND STAYS THE PERSON'S KEY AFTER IT LIFTS (2026-10-30 review 9): a reviewer's
/// drive holds the tab by a lease, the person's last typing two hours back. A `status` read that
/// sees the hand reads it as another person's key (DESIGN §15.4); the next one, the lease gone,
/// does not put the window's older typing stamp back, so the hour the hand holds outlives it. The
/// control: another harness process's lease is no key, and the typing stamp stands.
#[test]
fn a_drivers_hand_stays_the_key_after_it_lifts() {
    let typed = Duration::from_secs(2 * 3600);
    let ms = u64::try_from(typed.as_millis()).unwrap_or(u64::MAX);
    let key_after = |hand: &'static str| {
        let mut m = Mock::new(vec![idle_screen()]);
        m.human_ms = Some(ms);
        m.typed_ms = Some(ms);
        m.hand = hand;
        let mut s = session(&mut m, Some("@s-w".to_string()));
        assert!(s.foreground_program().is_ok(), "the hand's read");
        let seen = s.fx.mono();
        s.ctl.hand = "-";
        assert!(s.foreground_program().is_ok(), "the read after it lifts");
        s.key_ago(seen, &Screen::default())
    };
    let ago = key_after("lease:reviewer");
    assert!(
        ago.is_some_and(|ago| ago < Duration::from_secs(60)),
        "the hand's key once it lifts: {ago:?}"
    );
    let ago = key_after("lease:aterm-harness@1");
    assert!(
        ago.is_some_and(|ago| ago > typed - Duration::from_secs(60)),
        "NEGATIVE CONTROL: another harness process's lease: {ago:?}"
    );
}

/// THE HOUR FROM A TURN'S END HOLDS ACROSS A RESTART (2026-10-30 review 8): the person types as
/// a long turn of the agent's opens, and it ends on a park, so their hour runs from that point,
/// not from the key. The supervisor restarts minutes on, where the window's clock times the key
/// alone: the books carry the hour, and the go-ahead comes once it ends, never at the key's own
/// hour's end.
#[test]
fn the_hour_from_a_turns_end_holds_across_a_restart() {
    let dir = crate::supervise::test_scratch_path("go-ahead-hour", "restart");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let ledger = dir.join("s-1.jsonl");
    // The books' stamps run on the run's clock, as a restart reads them.
    let clock = virtual_clock();
    let now_ms = crate::supervise::journal::unix_ms();
    clock.set_at_ms(now_ms - now_ms.rem_euclid(1000));
    clock.set_running();
    clock.stamp_by_script();
    let run = |m: &mut Mock, budget: u64| {
        m.autopilot = Some("cruise");
        m.stall_sleep = Some(Duration::from_secs(60));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(budget)
        };
        record_watch_on(m, &opts, Arc::clone(&clock), |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        })
        .lines
    };
    // The turn runs 40 minutes after its first busy read, then parks; the loop ends 50 minutes
    // in, before the key's own hour is over.
    let mut first = Mock::new(vec![busy_screen(), parked("status?", "40m 0s")]);
    first.keyed_at_read = Some(0);
    first
        .stall_after_read
        .insert(1, Duration::from_secs(40 * 60));
    let before = run(&mut first, 50 * 60);
    let key = first.keyed_at.expect("the person's key");
    let (_, ended) = *first
        .read_times
        .iter()
        .find(|(i, _)| *i == 1)
        .unwrap_or_else(|| panic!("the park's read: {before:#?}"));
    clock.skip(Duration::from_secs(3 * 60));
    let restarted = clock.mono();
    // After the restart the agent answers each continuation with the same park.
    let mut m = Mock::new(vec![
        parked("keep going", "5s"),
        busy_screen(),
        parked("keep going", "5s"),
        busy_screen(),
        parked("keep going", "5s"),
    ]);
    m.turn_gates = vec![0, 2];
    m.keyed_at = Some(key);
    let lines = run(&mut m, 3 * 3600);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let typed: Vec<(&String, Instant)> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .zip(m.turn_times.iter().copied())
        .collect();
    let (_, at) = typed
        .iter()
        .find(|(r, _)| r.contains(GO_AHEAD_ANSWER))
        .unwrap_or_else(|| panic!("the go-ahead: {before:#?}\n{lines:#?}\n{rows}"));
    assert!(
        ended >= key + Duration::from_secs(10 * 60) && restarted < key + GO_AHEAD_HELD,
        "a long turn, and a restart within the key's own hour: {:?}, {:?}",
        ended.duration_since(key),
        restarted.duration_since(key)
    );
    assert!(
        *at >= ended + GO_AHEAD_HELD,
        "not within the hour from the turn's end ({:?} after the key, the turn {:?}): \
         {typed:#?}\n{rows}",
        at.duration_since(key),
        ended.duration_since(key)
    );
}

/// A SCROLL OR A CLICK IS NO KEY (2026-10-30 review 8): the window's `human_ms` times every
/// gesture, a wheel or a click in the tab among them, and its `human_typed_ms` the person's
/// typing alone. A person who scrolls as the agent's turn runs, their last typing two hours
/// back, holds no hour: the park gets the go-ahead once their grace is over. The control: the
/// same scroll from a window that reports no typing stamp holds the hour, as its `human_ms` is
/// all it says.
#[test]
fn a_scroll_holds_no_go_ahead_hour() {
    let run = |typed: Option<u64>| {
        let mut m = Mock::new(vec![busy_screen(), parked("status?", "2m 0s")]);
        m.autopilot = Some("cruise");
        m.keyed_at_read = Some(0);
        m.typed_ms = typed;
        m.stall_sleep = Some(Duration::from_secs(20));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..auto(10 * 60)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |_| {});
        let first = m
            .requests
            .iter()
            .find(|r| r.starts_with("turn "))
            .cloned()
            .unwrap_or_else(|| panic!("a turn: {lines:#?}"));
        first.contains(GO_AHEAD_ANSWER)
    };
    assert!(run(Some(2 * 3600 * 1000)), "the go-ahead at once");
    assert!(
        !run(None),
        "NEGATIVE CONTROL: a window with no typing stamp"
    );
}

/// THE LOOP'S KEY COUNTS THE MAC'S SLEEP AT THE WAKE (2026-10-30 review 9, second pass): the
/// person's key comes in the agent's turn, and the continuation's `status` reads time it, which
/// the loop keeps (`Session::key`); the same reply's hold then waits for the hour's end. As the
/// loop next reads the screen the lid closes for fifteen hours, which the window's typing age
/// counts and the loop's clock does not. The wake decides the park again on a key fifteen hours
/// old, so the go-ahead goes then, not at the hour's end in awake time.
#[test]
fn a_park_the_hour_held_gets_the_go_ahead_at_the_wake_after_a_sleep() {
    let held = parked("keep going", "5s");
    let mut m = Mock::new(vec![
        busy_screen(),
        parked("status?", "2m 0s"),
        busy_screen(),
        held.clone(),
        busy_screen(),
        held.clone(),
        held,
    ]);
    m.autopilot = Some("cruise");
    m.turn_gates = vec![1, 3];
    m.keyed_at_read = Some(0);
    m.typed_at_key = true;
    m.sleeps_at.insert(6, 15 * 3600);
    m.stall_sleep = Some(Duration::from_secs(20));
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..auto(3 * 3600)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    let key = m.keyed_at.expect("the person's key");
    assert!(
        m.sleeps_at.is_empty(),
        "PRECONDITION: the Mac slept: {lines:#?}"
    );
    let typed: Vec<(&String, Instant)> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .zip(m.turn_times.iter().copied())
        .collect();
    let (_, at) = typed
        .iter()
        .find(|(r, _)| r.contains(GO_AHEAD_ANSWER))
        .unwrap_or_else(|| panic!("the go-ahead: {lines:#?}\n{typed:#?}"));
    assert!(
        *at < key + GO_AHEAD_HELD,
        "at the wake, not at the hour's end in awake time ({:?} after the key): {typed:#?}",
        at.duration_since(key)
    );
}

/// THE LOOP'S OWN STAMPS OF THE PERSON COUNT THE MAC'S SLEEP (2026-10-30 review 9, second pass):
/// a `status` read times the person's key two minutes back and sees a reviewer's hand on the
/// tab, and the loop has seen a draft in the composer; then the Mac sleeps two hours, where
/// `Instant` stands still. At the wake every stamp the loop keeps of the person is two hours
/// older, as the window's ages are, so none reads as minutes old: not the key, not the hand
/// (`Session::hand_key`, which no later read takes back), not their last gesture or draft.
#[test]
fn the_loops_stamps_of_the_person_count_the_macs_sleep() {
    let slept = Duration::from_secs(2 * 3600);
    let mut m = Mock::new(vec![idle_screen()]);
    m.human_ms = Some(2 * 60 * 1000);
    m.typed_ms = Some(2 * 60 * 1000);
    m.hand = "lease:reviewer";
    let clock = virtual_clock();
    m.run_on(&clock);
    let mut s = session(&mut m, Some("@s-w".to_string()));
    s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
    s.set_clock(TEST_NOW, PDT);
    assert!(s.foreground_program().is_ok(), "the read before the sleep");
    s.draft_seen = Some(("wait, one more thing".to_string(), s.fx.mono()));
    clock.slept(i64::try_from(slept.as_secs()).unwrap_or(i64::MAX));
    s.watch_sleep();
    let now = s.fx.mono();
    let key = s.key_ago(now, &Screen::default());
    assert!(
        key.is_some_and(|ago| ago >= slept),
        "the key and the hand: {key:?}"
    );
    let person = s.person_ago(now);
    assert!(
        person.is_some_and(|ago| ago >= slept),
        "their gesture and draft: {person:?}"
    );
}
