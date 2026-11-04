// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A relaunched agent's owed carry-on at the screen its resume draws (owner report 2026-11-01:
//! "when you restarted, you didn't seem to start working again"). A restart cuts the tool call
//! that ran it, and Claude Code restores the cut call as `Interrupted · What should Claude do
//! instead?`, which reads as a person's Esc. A conversation that ended on the agent's own question
//! comes back on it, and that row reads as a question.

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::policy::turn_end::HostTurn;

const ESC_WORDS: &str = "a person stopped the turn with Esc";

/// The screen Claude Code 2.1.316 draws on resuming a conversation whose last act was the restart
/// its own tool call asked for.
fn restored() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Restarting onto the new build now.",
        "",
        "⏺ Bash(timeout 20 aterm ctl @self restart 2>&1 | head -3)",
        "  ⎿  Interrupted · What should Claude do instead?",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// The screen of a resumed conversation that ended on the agent's own question.
fn restored_on_its_question() -> Vec<String> {
    let mut r = rows(&["⏺ Which of the two should I land first?", ""]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// A host that owes a relaunch's carry-on while `owed` stands, and takes it at the first point it
/// is offered, as [`crate::harness::relaunch::resume_as`] does; it saw the agent launched at
/// `launch` (unix ms) where it is set.
#[derive(Debug, Default)]
struct Carrier {
    wants: AtomicBool,
    owed: AtomicBool,
    launch: Option<i64>,
    points: Mutex<Vec<IdlePoint>>,
}

impl Carrier {
    fn owing(owed: bool) -> Arc<Self> {
        let host = Self::default();
        host.wants.store(true, Ordering::SeqCst);
        host.owed.store(owed, Ordering::SeqCst);
        Arc::new(host)
    }

    /// A relaunch's host: the agent launched at `launch`, its carry-on owed.
    fn relaunched(launch: i64) -> Arc<Self> {
        let host = Self {
            launch: Some(launch),
            ..Self::default()
        };
        host.wants.store(true, Ordering::SeqCst);
        host.owed.store(true, Ordering::SeqCst);
        Arc::new(host)
    }

    fn points(&self) -> Vec<IdlePoint> {
        self.points.lock().unwrap().clone()
    }
}

impl IdleHost for Carrier {
    fn wants(&self) -> bool {
        self.wants.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.at_idle_on(IdlePoint::default())
    }
    fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        self.points.lock().unwrap().push(point);
        self.wants.store(false, Ordering::SeqCst);
        self.owed.swap(false, Ordering::SeqCst).then(|| HostStep {
            line: "carry-on step=continued".to_string(),
            moved: true,
            typed: Some(HostTurn::CarryOn),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn relaunch_owed(&self) -> bool {
        self.owed.load(Ordering::SeqCst)
    }
    fn launch(&self) -> Option<i64> {
        self.launch
    }
}

fn policy_session<'a>(m: &'a mut Mock, tag: &str) -> (Session<'a, Mock>, std::path::PathBuf) {
    let (dir, ledger) = ledger_at(tag);
    let mut s = session(m, Some("@s-1".to_owned()));
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    let mut notes = Vec::new();
    s.ledger = approvals::Ledger::open(Some(&ledger), Some("@s-1"), &mut notes);
    s.set_clock(TEST_NOW, PDT);
    s.set_mono_standing();
    s.set_clock_running();
    (s, dir)
}

/// THE RESUME'S OWN `Interrupted` ROW IS NO PERSON'S ESC. Nobody pressed Esc on the screen a
/// relaunch draws, so the person's grace holds nothing there, under Cruise and under Land.
/// CONTROL: with no carry-on owed the same row is a person's Esc, held for the grace, and only
/// then does the loop type, its plain continue with no word of a restart: what a relaunch
/// with its carry-on blocked at this screen would have got (`continue@v1` two minutes on).
#[test]
fn the_cut_call_a_resume_restores_is_no_persons_esc() {
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, "carry-no-esc");
        let mut opts = hosted(30);
        opts.policy.autopilot = autopilot;
        opts.idle_host = Some(Carrier::owing(true) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, restored(), 1);
        let act = s.turn_end_at_point(&point, None, &opts);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            !s.turn_end.persons_esc_stands(),
            "{autopilot:?}: the restored row read as a person's Esc: {act:?}"
        );
        assert!(
            !matches!(&act, Ok(TurnEndAction::WaitUntil { why, .. }) if why == ESC_WORDS),
            "{autopilot:?}: held for a person nobody saw: {act:?}"
        );
    }

    // The control, and whether the loop would ever have typed.
    let mut m = Mock::new(vec![busy_screen()]);
    let (mut s, dir) = policy_session(&mut m, "carry-esc-control");
    let mut opts = hosted(30);
    opts.idle_host = Some(Carrier::owing(false) as Arc<dyn IdleHost>);
    s.program = Some("claude".to_owned());
    let point = point_at(&s, restored(), 1);
    let act = s.turn_end_at_point(&point, None, &opts);
    assert!(s.turn_end.persons_esc_stands(), "the Esc stands: {act:?}");
    assert!(
        matches!(&act, Ok(TurnEndAction::WaitUntil { why, .. }) if why == ESC_WORDS),
        "the person's grace holds the point: {act:?}"
    );
    s.test_clock().skip(Duration::from_secs(121));
    let point = point_at(&s, restored(), 1);
    let act = s.turn_end_at_point(&point, None, &opts);
    let _ = std::fs::remove_dir_all(dir);
    match act {
        Ok(TurnEndAction::Type { text, rule_id }) => {
            assert!(
                !text.to_lowercase().contains("restart"),
                "the loop's own continue says nothing of a restart: {text}"
            );
            assert_eq!(
                rule_id,
                crate::supervise::policy::turn_end::RULE_CONTINUE,
                "{text}"
            );
        }
        other => panic!("past the grace the loop's continue is typed: {other:?}"),
    }
}

/// Run a session to `screen`, its caret on the composer, under a host that does or does not owe a
/// carry-on: the points it was offered, and what the loop said.
fn offered_at(screen: Vec<String>, owed: bool) -> (Vec<IdlePoint>, String) {
    let mut m = Mock::new(vec![busy_screen(), screen]);
    m.cursor_on_caret = true;
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(5));
    let host = Carrier::owing(owed);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let _ = s.watch(&opts, &mut out);
    (host.points(), String::from_utf8(out).expect("utf-8"))
}

/// THE OWED CARRY-ON STEPS AT THE ROW A CONVERSATION CAME BACK ON, though it reads as a question:
/// the host is offered the point, told that only its carry-on rests there. CONTROL: no carry-on
/// owed, and the host is offered no question, its upgrade or disk line never typed over one.
#[test]
fn the_owed_carry_on_is_offered_the_point_a_restored_row_reads_as_a_question() {
    let (points, out) = offered_at(restored_on_its_question(), true);
    assert_eq!(points.len(), 1, "offered once: {out}");
    assert!(points[0].carry_only, "only its carry-on rests there: {out}");
    let (points, out) = offered_at(restored_on_its_question(), false);
    assert_eq!(points, [], "no host step at a question nobody owes: {out}");
}

/// THE ROW A RESTART CUT IS AN ORDINARY REST (`Interrupted · What should Claude do instead?` is
/// the vendor's question to a person, never the agent asking): the owed carry-on is offered it as
/// any idle, and nothing is narrowed there.
#[test]
fn the_owed_carry_on_is_offered_the_cut_call_a_resume_restores() {
    let (points, out) = offered_at(restored(), true);
    assert_eq!(points.len(), 1, "offered once: {out}");
    assert!(!points[0].carry_only, "an idle, no question: {out}");
}

/// A host owing a carry-on that stands: each step takes nothing (`word`), and the carry-on owns the
/// turn ends while `holds` says so, as `WorkerIdle::carry_holds` does for its bounded wait.
#[derive(Debug)]
struct Standing {
    word: &'static str,
    holds: AtomicBool,
}

impl IdleHost for Standing {
    fn wants(&self) -> bool {
        true
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.at_idle_on(IdlePoint::default())
    }
    fn at_idle_on(&self, _: IdlePoint) -> Option<HostStep> {
        Some(HostStep {
            line: format!("carry-on step={}", self.word),
            moved: false,
            typed: None,
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        self.holds.load(Ordering::SeqCst)
    }
    fn relaunch_owed(&self) -> bool {
        true
    }
}

/// THE LOOP'S OWN ACT WAITS ON AN OWED CARRY-ON THAT STANDS: a restored row reads as no person's
/// Esc, and with the carry-on holding the turn ends the loop types nothing there, so the agent still
/// hears what ended. NEGATIVE CONTROL: its hold run out (`holds` false), Land's plain instruction is
/// typed at once and the carry-on later closes as carried: Cruise and Land never park on it.
/// MUTANT: `WorkerIdle::owns_turn_end` without `carry_holds` is the control, and the agent is never
/// told.
#[test]
fn the_loops_own_act_waits_on_an_owed_carry_on_that_stands() {
    let run = |autopilot: Autopilot, holds: bool, word: &'static str, tag: &str| {
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, tag);
        let mut opts = hosted(30);
        opts.policy.autopilot = autopilot;
        opts.idle_host = Some(Arc::new(Standing {
            word,
            holds: AtomicBool::new(holds),
        }) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, restored(), 1);
        let act = s.turn_end_at_point(&point, None, &opts);
        let _ = std::fs::remove_dir_all(dir);
        act
    };
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        for word in ["wait:unsettled", "busy:another-sweep", "wait:draft"] {
            let act = run(
                autopilot,
                true,
                word,
                &format!("carry-stands-{autopilot:?}"),
            );
            assert!(
                matches!(act, Ok(TurnEndAction::Nothing)),
                "{autopilot:?}, {word}: {act:?}"
            );
        }
    }
    let act = run(
        Autopilot::Land,
        false,
        "wait:unsettled",
        "carry-stands-past",
    );
    assert!(
        matches!(
            &act,
            Ok(TurnEndAction::Type { rule_id, .. })
                if *rule_id == crate::supervise::policy::turn_end::RULE_LAND
        ),
        "past its hold the loop acts: {act:?}"
    );
}

/// A RELAUNCH THAT OWES NO CARRY-ON GOES ON AT THE LOOP'S OWN FIRST POINT: a quiet move took the turn
/// end's point with no work running, so the new loop decides it as any other. Cruise types its plain
/// continue past the first short turn's back-off, and Land its instruction at once.
#[test]
fn a_relaunch_that_owes_no_carry_on_goes_on_at_the_loops_first_point() {
    let at_rest = || {
        let mut r = rows(&[
            "⏺ The parser reads the first half; the second half is next.",
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |autopilot: Autopilot, tag: &str| {
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, tag);
        let mut opts = hosted(30);
        opts.policy.autopilot = autopilot;
        opts.idle_host = Some(Carrier::owing(false) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, at_rest(), 1);
        let first = s.turn_end_at_point(&point, None, &opts);
        s.test_clock().skip(Duration::from_secs(121));
        let point = point_at(&s, at_rest(), 1);
        let later = s.turn_end_at_point(&point, None, &opts);
        let _ = std::fs::remove_dir_all(dir);
        (first, later)
    };
    let (first, later) = run(Autopilot::Cruise, "quiet-cruise");
    assert!(
        matches!(first, Ok(TurnEndAction::WaitUntil { .. })),
        "the first short turn's back-off: {first:?}"
    );
    assert!(
        matches!(
            &later,
            Ok(TurnEndAction::Type { rule_id, .. })
                if *rule_id == crate::supervise::policy::turn_end::RULE_CONTINUE
        ),
        "Cruise's continue: {later:?}"
    );
    let (first, _) = run(Autopilot::Land, "quiet-land");
    assert!(
        matches!(
            &first,
            Ok(TurnEndAction::Type { rule_id, .. })
                if *rule_id == crate::supervise::policy::turn_end::RULE_LAND
        ),
        "Land's instruction at once: {first:?}"
    );
}

/// A RESTART PROMPT'S SHORT REPLY MEASURES ITS BACK-OFF FROM THE REPLY, NOT FROM A POINT JUDGED BEFORE
/// IT: a prompt a draft or Off held has outlasted the back-off by the time it is typed, and the
/// loop's own prompt came on its heels (`+31 s` in the 2026-11-01 re-check, and again for
/// `continued:check` in run rvd1 of 2026-11-02) while the clock had run out with nothing able to be
/// typed. A restart prompt is typed either way, by what the agent's last words claimed (a carry-on,
/// or the done check's), and its reply is read either way, as work going on or as a claim of done.
/// CONTROL: past its back-off from that reply the loop's own prompt comes, as Cruise's every turn end
/// does, so nothing stops the loop.
#[test]
fn a_held_carry_ons_short_reply_measures_the_back_off_from_the_reply() {
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, RULE_DONE_CHECK};
    const DONE: &str = "All tasks are complete and verified; nothing is left to do.";
    const HALF: &str = "The parser reads the first half.";
    const GOES_ON: &str = "Picked it up; the second half is next.";
    let at_rest = |said: &str| {
        let mut r = rows(&[&format!("⏺ {said}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let backoff = crate::supervise::policy::turn_end::TurnEndTiming::default().short_backoff;
    for (case, kind, before, answer, then) in [
        ("carry-on", HostTurn::CarryOn, HALF, GOES_ON, RULE_CONTINUE),
        ("check, work", HostTurn::Check, DONE, GOES_ON, RULE_CONTINUE),
        ("check, done", HostTurn::Check, DONE, DONE, RULE_DONE_CHECK),
    ] {
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, "held-carry-clock");
        let mut opts = hosted(30);
        opts.policy.autopilot = Autopilot::Cruise;
        opts.idle_host = Some(Carrier::owing(false) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, at_rest(before), 1);
        let first = s.turn_end_at_point(&point, None, &opts);
        assert!(
            matches!(first, Ok(TurnEndAction::WaitUntil { .. })),
            "{case}: the first short turn's back-off: {first:?}"
        );
        // The hold: the back-off runs out while nothing can be typed.
        s.test_clock().skip(backoff + Duration::from_secs(1));
        let typed = s.fx.mono();
        s.turn_end.host_typed(typed, kind);
        let worked = Duration::from_secs(3);
        s.test_clock().skip(worked);
        let reply = s.fx.mono();
        let point = point_at(&s, at_rest(answer), 2);
        let after = s.turn_end_at_point(&point, Some(worked), &opts);
        assert!(
            matches!(&after, Ok(TurnEndAction::WaitUntil { until, .. }) if *until >= reply + backoff),
            "{case}: the back-off from the reply: {after:?}"
        );
        s.test_clock().skip(backoff);
        let point = point_at(&s, at_rest(answer), 2);
        let later = s.turn_end_at_point(&point, None, &opts);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            matches!(&later, Ok(TurnEndAction::Type { rule_id, .. }) if *rule_id == then),
            "{case}: CONTROL: Cruise goes on past it: {later:?}"
        );
    }
}

/// A TURN A RESTART CUT IS NO SHORT ANSWER OF THE AGENT'S (v6 §14.17.5.7): a read caught the turn
/// running, the restart ended it seconds in, and the resumed conversation restores it as the
/// `Interrupted` row its carry-on is owed at. Nobody's prompt was answered short there, so the streak
/// stands at the short answers before it, and the carry-on's short reply backs off on that rung, as
/// it does where no read caught the cut turn. The 2026-11-03 real-Claude flows on main read 2 min
/// after the carry-on's reply where the restart came from inside the call (no read caught it) and
/// 4 min where it came from outside (a read did). The same holds for the agent's answer to Cruise's
/// own `continue@v1` that the restart cut, the common case of an agent restarting itself, whether
/// this loop typed it or a loop before this one did (its yield seeded).
/// CONTROL: the same short turn ended by the agent itself, at rest, counts.
#[test]
fn a_turn_a_restart_cut_adds_nothing_to_the_short_streak() {
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, TurnEndTiming};
    /// Whose turn the restart cuts.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Turn {
        /// Someone's own, no act of aterm's awaited.
        Theirs,
        /// The answer to the `continue@v1` this loop typed.
        Continue,
        /// The answer to the `continue@v1` a loop before this one typed.
        SeededContinue,
    }
    let at_rest = |said: &str| {
        let mut r = rows(&[&format!("⏺ {said}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let backoff = TurnEndTiming::default().short_backoff;
    const HI: &str = "Hi! I am the local lab stub, not a model.";
    let run = |turn: Turn, cut: bool, tag: &str| {
        let answers_continue = turn == Turn::Continue;
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, tag);
        let mut opts = hosted(30);
        opts.policy.autopilot = Autopilot::Cruise;
        let host = Carrier::owing(false);
        opts.idle_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, at_rest(HI), 1);
        let first = s.turn_end_at_point(&point, None, &opts);
        assert!(
            matches!(first, Ok(TurnEndAction::WaitUntil { .. })),
            "{tag}: PRECONDITION: the first short turn's back-off: {first:?}"
        );
        assert_eq!(s.turn_end.short_streak(), 1, "{tag}: PRECONDITION");
        if answers_continue {
            // Past the back-off Cruise types its own continue, and the turn is its answer.
            s.test_clock().skip(backoff + Duration::from_secs(1));
            let point = point_at(&s, at_rest(HI), 1);
            let act = s
                .turn_end_at_point(&point, None, &opts)
                .expect("a decision");
            assert_eq!(act.rule_id(), Some(RULE_CONTINUE), "{tag}: PRECONDITION");
            let r = s.turn_end_reread(&point.screen, &opts);
            let typed = s.fx.mono();
            s.turn_end.acted(&act, &r, typed);
        }
        if turn == Turn::SeededContinue {
            s.turn_end.seed_yield(RULE_CONTINUE);
        }
        // A turn runs three seconds: a restart cuts it, or the agent ends it.
        s.test_clock()
            .skip(Duration::from_secs(if answers_continue { 3 } else { 10 }));
        host.owed.store(cut, Ordering::SeqCst);
        host.wants.store(cut, Ordering::SeqCst);
        let screen = if cut {
            restored()
        } else {
            at_rest("Tool call done.")
        };
        let point = point_at(&s, screen, 2);
        let at = s.turn_end_at_point(&point, Some(Duration::from_secs(3)), &opts);
        let streak = s.turn_end.short_streak();
        let after_reply = (cut && turn == Turn::Theirs).then(|| {
            // The carry-on typed there, and its short reply.
            let typed = s.fx.mono();
            s.turn_end.host_typed(typed, HostTurn::CarryOn);
            let worked = Duration::from_secs(1);
            s.test_clock().skip(worked);
            let reply = s.fx.mono();
            let point = point_at(&s, at_rest("Tool call done."), 3);
            (reply, s.turn_end_at_point(&point, Some(worked), &opts))
        });
        let _ = std::fs::remove_dir_all(dir);
        (streak, at, after_reply)
    };
    let (streak, at, after_reply) = run(Turn::Theirs, true, "restart-cut-theirs");
    assert_eq!(streak, 1, "the turn the restart cut was counted: {at:?}");
    let (reply, after) = after_reply.expect("the carry-on's reply");
    assert!(
        matches!(&after, Ok(TurnEndAction::WaitUntil { until, .. })
            if *until >= reply + backoff && *until < reply + backoff * 2),
        "the carry-on's short reply backs off on the first rung: {after:?}"
    );
    for (turn, tag) in [
        (Turn::Continue, "restart-cut-continue"),
        (Turn::SeededContinue, "restart-cut-seeded"),
    ] {
        let (streak, at, _) = run(turn, true, tag);
        assert_eq!(
            streak, 1,
            "{turn:?}: the answer the restart cut was counted: {at:?}"
        );
    }
    for (turn, tag) in [
        (Turn::Theirs, "restart-cut-theirs-control"),
        (Turn::Continue, "restart-cut-continue-control"),
        (Turn::SeededContinue, "restart-cut-seeded-control"),
    ] {
        let (streak, at, _) = run(turn, false, tag);
        assert_eq!(
            streak, 2,
            "CONTROL {turn:?}: a short turn the agent ended counts: {at:?}"
        );
    }
}

/// THE LOOP A RELAUNCH STARTS COUNTS NOTHING FOR THE CUT AT ITS FIRST POINT EITHER: its books hold
/// the point the loop before it judged (one short turn) and the turn that loop read running after it
/// (the restart's own call), and its first point, no work read, is the `Interrupted` row the resume
/// draws while the carry-on is owed. The 2026-11-03 real-Claude flow f2 on main recorded that point
/// as a second short turn, so the carry-on's short reply backed off 4 min where flow f1, whose
/// restart no read caught (the books then seed the point, `TurnEndState::seed_point`), backed off 2.
/// CONTROL: the same loop, books and host at the agent's own short reply counts it, and the carry-on's
/// reply backs off on that second rung.
#[test]
fn the_loop_a_relaunch_starts_counts_nothing_for_the_cut_at_its_first_point() {
    let (_, books) = launched_run(
        "restored-cut-books",
        &mut Mock::new(vec![claude_pre_banner(), busy_screen()]),
        &LaunchHost::saw(None),
        Some(""),
    );
    assert_eq!(
        point_rows(&books),
        [(101, 1)],
        "PRECONDITION: the books: {books:#?}"
    );
    assert!(
        books
            .last()
            .is_some_and(|r| r.contains(approvals::RULE_TURN_RAN)),
        "PRECONDITION: the turn read running: {books:#?}"
    );
    let books = books.join("\n") + "\n";
    let last = approvals::last_row_ms(&books).expect("the books' last row");
    let run = |first: Vec<String>, tag: &str| {
        let host = Carrier::relaunched(last + 1_000);
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, &books).expect("the ledger");
        let journal = dir.join("journal.jsonl");
        let mut m = Mock::new(vec![first, busy_screen(), ended_quick(LAB_REPLY)]);
        m.cursor_on_caret = true;
        m.vanish_after = Some(3);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..hosted(60)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let lines: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .collect();
        let rows: Vec<String> = std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        let streaks: Vec<u32> = point_rows(&rows)
            .into_iter()
            .skip(1)
            .map(|(_, n)| n)
            .collect();
        (host.points().len(), streaks, back_offs(&lines), lines)
    };
    let (offered, streaks, waits, lines) = run(restored(), "restored-cut-new-loop");
    assert_eq!(
        offered, 1,
        "PRECONDITION: the carry-on is offered its point: {lines:#?}"
    );
    assert_eq!(
        streaks.first(),
        Some(&1),
        "the cut was counted: {streaks:?}\n{lines:#?}"
    );
    assert_eq!(
        waits.last().map(String::as_str),
        Some("1 short turn(s) in a row: 2 min back-off"),
        "the carry-on's short reply: {lines:#?}"
    );
    let (offered, streaks, waits, lines) =
        run(ended_quick(LAB_REPLY), "restored-cut-new-loop-control");
    assert_eq!(offered, 1, "CONTROL PRECONDITION: {lines:#?}");
    assert_eq!(
        streaks.first(),
        Some(&2),
        "CONTROL: the agent's own short reply counts: {streaks:?}\n{lines:#?}"
    );
    assert_eq!(
        waits.last().map(String::as_str),
        Some("2 short turn(s) in a row: 4 min back-off"),
        "CONTROL: {lines:#?}"
    );
}

/// THE RESTORE LINE'S SHORT REPLY MEASURES ITS BACK-OFF FROM THE REPLY AS WELL: `continued:restore`
/// is the one line a restart types where the resumed build warned something was lost, as much a
/// restart's prompt as the two above and held behind a draft or Off the same way. Its reply stays
/// no done report (`a_quiet_restarts_line_answered_nothing_else_is_no_done_report`). CONTROL: an
/// upgrade's `announced` asks again at its own pace, so a run of its short replies moves nothing
/// and the loop's own prompt comes at once.
#[test]
fn a_restore_lines_short_reply_measures_the_back_off_from_the_reply() {
    use crate::harness::upgrade_drive::host_turn;
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, TurnEndTiming};
    const REPLY: &str = "Restored nothing: no watch, monitor or loop was running.";
    let at_rest = |said: &str| {
        let mut r = rows(&[&format!("⏺ {said}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let backoff = TurnEndTiming::default().short_backoff;
    for (step, from_the_reply) in [("continued:restore", true), ("announced:1", false)] {
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, "held-restore-clock");
        let mut opts = hosted(30);
        opts.policy.autopilot = Autopilot::Cruise;
        opts.idle_host = Some(Carrier::owing(false) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, at_rest("The parser reads the first half."), 1);
        let first = s.turn_end_at_point(&point, None, &opts);
        assert!(
            matches!(first, Ok(TurnEndAction::WaitUntil { .. })),
            "{step}: the first short turn's back-off: {first:?}"
        );
        // The hold: the back-off runs out while nothing can be typed.
        s.test_clock().skip(backoff + Duration::from_secs(1));
        let typed = s.fx.mono();
        s.turn_end
            .host_typed(typed, host_turn(step).expect("a typed step"));
        let worked = Duration::from_secs(3);
        s.test_clock().skip(worked);
        let reply = s.fx.mono();
        let point = point_at(&s, at_rest(REPLY), 2);
        let after = s.turn_end_at_point(&point, Some(worked), &opts);
        if from_the_reply {
            assert!(
                matches!(&after, Ok(TurnEndAction::WaitUntil { until, .. }) if *until >= reply + backoff),
                "{step}: the back-off from the reply: {after:?}"
            );
            s.test_clock().skip(backoff);
            let point = point_at(&s, at_rest(REPLY), 2);
            let later = s.turn_end_at_point(&point, None, &opts);
            assert!(
                matches!(&later, Ok(TurnEndAction::Type { rule_id, .. }) if *rule_id == RULE_CONTINUE),
                "{step}: Cruise goes on past it: {later:?}"
            );
            assert_eq!(s.turn_end.done_reports(), 0, "{step}: no done report");
        } else {
            assert!(
                matches!(&after, Ok(TurnEndAction::Type { rule_id, .. }) if *rule_id == RULE_CONTINUE),
                "CONTROL {step}: the clock stayed on the earlier point: {after:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A RESTART PROMPT'S REPLY NO READ SAW RUN IS JUDGED AT THE READ THAT SHOWS IT ANSWERED: the
/// screen closes its turn on the prompt the harness typed, so its short reply backs off from there
/// at once. The 2026-11-03 real-Claude flow f2fix (Claude Code 2.1.317, lab stub) read `⏺ Tool call
/// done.` under the restart prompt half a second after typing it, and the loop still waited 30 s on
/// `the worker to take the harness-turn@v1 act` (`TurnEndTiming::take_within`) before it judged the
/// reply. Under Cruise the screen the host types past usually closes on Cruise's own continuation,
/// which opens with the same harness mark as the restart prompt. CONTROLS, each waiting on the
/// worker to take the act, the hang detector for a turn no screen shows answered (past it the reply
/// is judged all the same): a read that shows the restart prompt with no answer under it yet, and
/// a read that still closes on that continuation, answered when the host typed and not the restart
/// prompt's reply, the restart prompt's line not yet drawn.
#[test]
fn a_restart_prompts_reply_no_read_saw_run_is_judged_when_the_screen_shows_it() {
    use crate::supervise::policy::turn_end::{TurnEndTiming, WaitKind};
    const RESTARTED: &str = "[aterm harness] Restarted: someone asked aterm to restart Claude Code \
        (aterm ctl restart), so this session was ended and restarted on Claude Code 2.1.317 and \
        resumed.";
    const TAKE: &str = "the worker to take the harness-turn@v1 act";
    let screen = |turns: &[(&str, Option<&str>)]| {
        let mut r = Vec::new();
        for (prompt, answer) in turns {
            r.push(format!("❯ {prompt}"));
            if let Some(answer) = answer {
                r.push(format!("⏺ {answer}"));
                r.push(String::new());
                r.push("✻ Churned for 0s · done 5:10 AM".to_string());
            }
            r.push(String::new());
        }
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let hi = ("hi", Some("Hi! I am the local lab stub, not a model."));
    let carried = (
        "[aterm harness] Continue the task.",
        Some("Nothing is left to do here."),
    );
    let timing = TurnEndTiming::default();
    #[derive(Clone, Copy, Debug)]
    enum Read {
        Answered,
        Unanswered,
        Undrawn,
    }
    for read in [Read::Answered, Read::Unanswered, Read::Undrawn] {
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut s, dir) = policy_session(&mut m, "restart-reply-unseen");
        let mut opts = hosted(30);
        opts.policy.autopilot = Autopilot::Cruise;
        opts.idle_host = Some(Carrier::owing(false) as Arc<dyn IdleHost>);
        s.program = Some("claude".to_owned());
        let point = point_at(&s, screen(&[hi, carried]), 1);
        let first = s.turn_end_at_point(&point, None, &opts);
        assert!(
            matches!(first, Ok(TurnEndAction::WaitUntil { .. })),
            "PRECONDITION ({read:?}): the first short turn's back-off: {first:?}"
        );
        s.test_clock()
            .skip(timing.short_backoff + Duration::from_secs(1));
        let typed = s.fx.mono();
        s.turn_end.host_typed(typed, HostTurn::CarryOn);
        s.test_clock().skip(Duration::from_secs(1));
        let reply = s.fx.mono();
        let rows = match read {
            Read::Answered => screen(&[hi, carried, (RESTARTED, Some("Tool call done."))]),
            Read::Unanswered => screen(&[hi, carried, (RESTARTED, None)]),
            Read::Undrawn => screen(&[hi, carried]),
        };
        let point = point_at(&s, rows.clone(), 2);
        let after = s.turn_end_at_point(&point, None, &opts);
        let backs_off = |a: &Result<TurnEndAction, Fail>, from: Instant| {
            matches!(a, Ok(TurnEndAction::WaitUntil { until, wait: WaitKind::BackOff { .. }, .. })
                if *until >= typed + timing.short_backoff
                    && *until <= from + timing.short_backoff)
        };
        if matches!(read, Read::Answered) {
            assert!(
                backs_off(&after, reply),
                "the reply is judged at the read that shows it, its back-off from the reply: \
                 {after:?}"
            );
        } else {
            assert!(
                matches!(&after, Ok(TurnEndAction::WaitUntil { why, .. }) if why == TAKE),
                "CONTROL ({read:?}): no answer to the restart prompt shown, the worker is waited \
                 on: {after:?}"
            );
            s.test_clock().skip(timing.take_within);
            let due = s.fx.mono();
            let point = point_at(&s, rows, 3);
            let past = s.turn_end_at_point(&point, None, &opts);
            assert!(
                backs_off(&past, due),
                "CONTROL ({read:?}): past the hang detector the point is judged: {past:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
