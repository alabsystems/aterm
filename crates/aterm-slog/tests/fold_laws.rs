// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold's laws (§7.1, §3.7) and the clause (§7.3).

use aterm_slog::{
    Actor, Amend, BoxWord, Close, Conf, Fields, Fold, Key, Kind, Limits, Log, Mark, Open, Out, Rec,
    RunOut, ShortId, Src, Tone, TurnTracker, Verdict, WallKind,
};

use crate::support::{WALL0, agent_session, at};

/// The same traffic into a two-record ring and a full-sized one.
fn traffic(log: &mut Log) {
    traffic_from(log, 0);
}

/// [`traffic`] starting at `ms0`.
fn traffic_from(log: &mut Log, ms0: u64) {
    let run = agent_session(log, ms0);
    let mut tr = TurnTracker::new(run);
    let mut ms = ms0;
    let see = |log: &mut Log, tr: &mut TurnTracker, ms: &mut u64, v: Verdict| {
        *ms += 100;
        tr.observe(log, v, Some("3pm"), at(*ms));
    };
    for _ in 0..3 {
        see(log, &mut tr, &mut ms, Verdict::Busy);
        see(
            log,
            &mut tr,
            &mut ms,
            Verdict::Prompt(BoxWord::from_detail("edit")),
        );
        see(log, &mut tr, &mut ms, Verdict::Busy);
        see(log, &mut tr, &mut ms, Verdict::Idle);
        ms += 1500;
        tr.tick(log, at(ms));
    }
    see(log, &mut tr, &mut ms, Verdict::Busy);
    see(log, &mut tr, &mut ms, Verdict::Wall(WallKind::UsageSession));
    log.mark(
        Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
            .fields(Fields::new().word(Key::Dir, "in").expect("dir")),
        at(ms + 1),
    )
    .expect("mail");
}

#[test]
fn eviction_never_changes_the_state() {
    let mut small = Log::with_limits(0, Limits::new(2, 1 << 20).expect("tiny"));
    let mut big = Log::new(0);
    traffic(&mut small);
    traffic(&mut big);
    assert!(small.lost() > 0 && big.lost() == 0);
    // The one difference is a fact about the ring, not the session: a close
    // record's `lost=` rollup counts its node's records the ring evicted.
    let strip = |log: &Log| {
        let mut st = log.state().clone();
        for node in st.last.iter_mut().flatten() {
            node.f.remove(Key::Lost);
        }
        st
    };
    assert_eq!(strip(&small), strip(&big));
    assert_ne!(small.state(), big.state(), "the rollup did count the loss");
    assert_eq!(small.state().totals.turns, 4);
    assert_eq!(small.state().totals.done, 3);
    assert_eq!(small.state().mail_open, 1);
}

#[test]
fn apply_is_idempotent_by_seq() {
    let mut log = Log::new(0);
    traffic(&mut log);
    let mut once = Fold::new(0);
    let mut twice = Fold::new(0);
    for r in log.window() {
        once.apply(r);
        twice.apply(r);
        twice.apply(r);
    }
    for r in log.window() {
        twice.apply(r);
    }
    assert_eq!(once, twice);
    assert_eq!(once.state(), log.state());
}

/// Idempotent across an update too: a window that starts with the
/// successor's `gap why=handoff` folds the same once or twice, and a fold
/// that read the old epoch crosses into the new one once, however often it
/// is handed the same records (the bus delivers duplicates).
#[test]
fn apply_is_idempotent_across_a_handoff() {
    let mut old = Log::new(2);
    traffic(&mut old);
    let mut new = Log::after_handoff(3, Limits::SESSION, old.id_counters(), 1, at(90_000));
    traffic_from(&mut new, 90_000);
    let fold_of = |passes: &[&[Rec]]| {
        let mut f = Fold::new(0);
        for pass in passes {
            for r in *pass {
                f.apply(r);
            }
        }
        f
    };
    let old_w: Vec<Rec> = old.window().cloned().collect();
    let new_w: Vec<Rec> = new.window().cloned().collect();
    assert_eq!(new_w[0].f.get_word(Key::Why), Some("handoff"));
    let once = fold_of(&[&new_w]);
    assert_eq!(fold_of(&[&new_w, &new_w]), once);
    assert_eq!(once.state().ep, 3);
    assert_eq!(once.state().totals.turns, new.state().totals.turns);

    let crossed = fold_of(&[&old_w, &new_w]);
    assert_eq!(fold_of(&[&old_w, &new_w, &new_w]), crossed);
    assert_eq!(
        fold_of(&[&old_w, &new_w, &old_w, &new_w]),
        crossed,
        "a stale epoch starts nothing"
    );
    assert_eq!(crossed.state().ep, 3);
    assert_eq!(crossed.state().as_of, new.last_seq());
    // The old epoch's turns count once, the new one's once.
    assert_eq!(
        crossed.state().totals.turns,
        old.state().totals.turns + new.state().totals.turns
    );
}

/// A claim never outranks an observation (§3.7, §7.1): a `need` mark raises
/// only `review`, `decision` or `help` (`escalated` only from the owner's
/// attention flag), and ranks below every observed need.
#[test]
fn a_claimed_need_never_displaces_an_observed_one() {
    let now = |ms: u64| WALL0 + ms;
    let mut log = Log::new(0);
    let r = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Exact), at(0))
        .expect("cmd");
    log.close(
        r,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact)
            .fields(Fields::new().i(Key::Code, 101).expect("code")),
        at(5),
    )
    .expect("closes");
    let need = |log: &mut Log, word: &str, src: Src, conf: Conf, ms: u64| {
        log.mark(
            Mark::new(Kind::Need, src, conf)
                .by(Actor::Sid(ShortId::new("s-other").expect("sid")))
                .fields(Fields::new().word(Key::Word, word).expect("word")),
            at(ms),
        )
        .expect("mark")
    };
    for word in ["unresponsive", "failure", "exited", "escalated"] {
        need(&mut log, word, Src::Mark, Conf::Claim, 6);
    }
    let words: Vec<&str> = log.state().needs.iter().flatten().map(|n| n.word).collect();
    assert_eq!(
        words,
        vec!["failure"],
        "a mark raises no observed need's word"
    );
    need(&mut log, "help", Src::Mark, Conf::Claim, 7);
    let c = log.fold().clause(now(8)).expect("clause");
    assert_eq!(
        (c.words.as_str(), c.tone),
        ("failed · exit 101", Tone::Failed)
    );
    let words: Vec<&str> = log.state().needs.iter().flatten().map(|n| n.word).collect();
    assert_eq!(words, vec!["failure", "help"]);
    // The owner's attention flag escalates, above `exited`, below a failure.
    need(&mut log, "escalated", Src::Meta, Conf::Strong, 9);
    let words: Vec<&str> = log.state().needs.iter().flatten().map(|n| n.word).collect();
    assert_eq!(words, vec!["failure", "escalated", "help"]);
}

#[test]
fn needs_rank_by_precedence_not_arrival() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(Actor::Harness)
            .fields(Fields::new().word(Key::Word, "review").expect("need")),
        at(2),
    )
    .expect("mark");
    log.open(
        Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(turn),
        at(3),
    )
    .expect("stall");
    let f = Fields::new().word(Key::Box, "bash:read-only").expect("box");
    let appr = log
        .open(
            Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                .under(turn)
                .fields(f),
            at(4),
        )
        .expect("approval");
    let words: Vec<&str> = log.state().needs.iter().flatten().map(|n| n.word).collect();
    assert_eq!(words, vec!["approval:bash", "unresponsive", "review"]);
    log.close(
        appr,
        Close::new(
            Out::Step(aterm_slog::StepOut::Approved),
            Src::Screen,
            Conf::Strong,
        )
        .by(Actor::Harness),
        at(5),
    )
    .expect("approved");
    assert_eq!(log.state().top_need().map(|n| n.word), Some("unresponsive"));
    assert_eq!(log.state().totals.appr_by, [0, 1, 0, 0]);

    // A clear lifts a mark's need only from its raiser or the owner.
    for (by, lifted) in [(Actor::Bridge, false), (Actor::Harness, true)] {
        log.mark(
            Mark::new(Kind::Clear, Src::Mark, Conf::Claim)
                .by(by)
                .fields(Fields::new().word(Key::Word, "review").expect("word")),
            at(6),
        )
        .expect("clear");
        let held = log
            .state()
            .needs
            .iter()
            .flatten()
            .any(|n| n.word == "review");
        assert_eq!(held, !lifted, "a clear by {by:?}");
    }
    // A clear never lifts a step's need: the step holds it.
    log.mark(
        Mark::new(Kind::Clear, Src::Mark, Conf::Claim).by(Actor::Human),
        at(7),
    )
    .expect("clear all");
    assert_eq!(log.state().top_need().map(|n| n.word), Some("unresponsive"));
}

#[test]
fn the_confidence_law_keeps_the_most_reliable_value() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    let amend = |log: &mut Log, src: Src, conf: Conf, ms: u64, n: u64| {
        let f = Fields::new().u(Key::TokIn, n).expect("u");
        log.amend(
            turn,
            Amend::new(Kind::Turn, src, conf, f).by(Actor::Vendor),
            at(ms),
        )
        .expect("amend")
    };
    let held = |log: &Log| log.state().turn().and_then(|t| t.f.u(Key::TokIn));
    amend(&mut log, Src::Screen, Conf::Heuristic, 2, 10);
    assert_eq!(held(&log), Some(10));
    amend(&mut log, Src::Transcript, Conf::Exact, 3, 20);
    assert_eq!(held(&log), Some(20), "exact replaces heuristic");
    amend(&mut log, Src::Screen, Conf::Strong, 4, 30);
    assert_eq!(held(&log), Some(20), "a later screen read cannot undo it");
    amend(&mut log, Src::Transcript, Conf::Exact, 5, 40);
    assert_eq!(held(&log), Some(40), "ties go to the later seq");
    amend(&mut log, Src::Mark, Conf::Claim, 6, 50);
    assert_eq!(
        held(&log),
        Some(40),
        "a claim never overrides an observation"
    );
    assert!(aterm_slog::confidence_law(
        Conf::Claim,
        9,
        Conf::Heuristic,
        1
    ));
    assert!(!aterm_slog::confidence_law(
        Conf::Heuristic,
        1,
        Conf::Claim,
        9
    ));
}

/// Equal field counts can retain different amounts of owned text. The
/// estimate must follow that storage in both open and closed fold state;
/// numeric magnitudes and static word lengths do not allocate more space.
#[test]
fn retained_heap_grows_with_owned_values_but_not_inline_values() {
    let node_sizes = |fields: Fields| {
        let mut log = Log::new(0);
        let run = log
            .open(
                Open::new(Kind::Cmd, Src::Shell, Conf::Exact).fields(fields),
                at(0),
            )
            .unwrap();
        let open = log.fold().footprint();
        log.close(
            run,
            Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
            at(1),
        )
        .unwrap();
        (open, log.fold().footprint())
    };
    for (short, long) in [
        (
            Fields::new().tok(Key::Prog, "x").unwrap(),
            Fields::new().tok(Key::Prog, &"x".repeat(32)).unwrap(),
        ),
        (
            Fields::new().text(Key::Cwd, "x").unwrap(),
            Fields::new().text(Key::Cwd, &"x".repeat(128)).unwrap(),
        ),
    ] {
        let small = node_sizes(short);
        let large = node_sizes(long);
        assert!(large.0 > small.0, "open fields retain their owned content");
        assert!(
            large.1 > small.1,
            "closed fields retain their owned content"
        );
    }
    for (small, large) in [
        (
            Fields::new().u(Key::Blk, 1).unwrap(),
            Fields::new().u(Key::Blk, u64::MAX).unwrap(),
        ),
        (
            Fields::new().word(Key::Word, "busy").unwrap(),
            Fields::new().word(Key::Word, "question").unwrap(),
        ),
    ] {
        assert_eq!(node_sizes(small), node_sizes(large), "inline value control");
    }
    let mark_size = |kind, fields| {
        let mut log = Log::new(0);
        log.mark(
            Mark::new(kind, Src::Mark, Conf::Claim).fields(fields),
            at(0),
        )
        .unwrap();
        log.fold().footprint()
    };
    let failure_size = |code| {
        let mut log = Log::new(0);
        let run = log
            .open(Open::new(Kind::Cmd, Src::Shell, Conf::Exact), at(0))
            .unwrap();
        log.close(
            run,
            Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact)
                .fields(Fields::new().i(Key::Code, code).unwrap()),
            at(1),
        )
        .unwrap();
        log.fold().footprint()
    };
    assert!(
        failure_size(i64::MIN) > failure_size(1),
        "a failure retains its numeric code as owned need detail"
    );
    assert!(
        mark_size(
            Kind::Label,
            Fields::new().text(Key::Text, &"x".repeat(128)).unwrap()
        ) > mark_size(Kind::Label, Fields::new().text(Key::Text, "x").unwrap()),
        "retained claim text"
    );
}

#[test]
fn silence_raises_no_need() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
        at(1),
    )
    .expect("turn");
    let needs = log.state().needs.clone();
    for quiet in [0u64, 60_000, 3_600_000, 86_400_000] {
        let now = WALL0 + 1 + quiet;
        assert_eq!(log.fold().hints(now).quiet_for_ms, quiet);
        assert_eq!(log.state().needs, needs);
        let c = log.fold().clause(now).expect("a clause");
        assert_eq!(c.tone, Tone::Working, "long quiet is not stuck");
    }
}

#[test]
fn clause_rows_in_order() {
    let now = |ms: u64| WALL0 + ms;
    let mut log = Log::new(0);
    let f = Fields::new()
        .tok(Key::Detail, "cargo build")
        .expect("detail");
    let cmd = log
        .open(
            Open::new(Kind::Cmd, Src::Shell, Conf::Exact).fields(f),
            at(0),
        )
        .expect("cmd");
    let c = log.fold().clause(now(120_000)).expect("clause");
    assert_eq!(
        (c.words.as_str(), c.tone),
        ("cargo build · 2m", Tone::Working)
    );

    let run = log
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong).under(cmd),
            at(1),
        )
        .expect("agent");
    assert_eq!(
        log.fold()
            .clause(now(2))
            .map(|c| c.words.as_str().to_string()),
        Some("idle".to_string())
    );
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(10),
        )
        .expect("turn");
    assert_eq!(
        log.fold()
            .clause(now(240_010))
            .map(|c| c.words.as_str().to_string()),
        Some("working · 4m".to_string())
    );
    let f = Fields::new().word(Key::Cls, "edit").expect("cls");
    log.open(
        Open::new(Kind::Tool, Src::Transcript, Conf::Exact)
            .under(turn)
            .fields(f),
        at(20),
    )
    .expect("tool");
    assert_eq!(
        log.fold()
            .clause(now(60_020))
            .map(|c| c.words.as_str().to_string()),
        Some("editing · 1m".to_string())
    );

    // A label from a session (not the harness or the owner) is not shown;
    // the harness's is, until it expires.
    let label = |log: &mut Log, by: Actor, ms: u64| {
        log.mark(
            Mark::new(Kind::Label, Src::Mark, Conf::Claim)
                .by(by)
                .fields(
                    Fields::new()
                        .text(Key::Text, "fixing the parser")
                        .and_then(|f| f.u(Key::Ttl, 60))
                        .expect("label"),
                ),
            at(ms),
        )
        .expect("label")
    };
    label(&mut log, Actor::Sid(ShortId::new("s-1").expect("sid")), 30);
    assert!(
        log.fold()
            .clause(now(31))
            .is_some_and(|c| c.words.as_str().starts_with("editing"))
    );
    label(&mut log, Actor::Harness, 40);
    let c = log.fold().clause(now(40 + 30_000)).expect("clause");
    assert_eq!(
        (c.words.as_str(), c.by),
        ("fixing the parser · 30s", Actor::Harness)
    );
    assert!(
        log.fold()
            .clause(now(40 + 61_000))
            .is_some_and(|c| c.words.as_str().starts_with("editing")),
        "the label expired"
    );

    // A failed run needs someone, and says how it failed.
    let mut log = Log::new(0);
    let r = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Exact), at(0))
        .expect("cmd");
    log.close(
        r,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact)
            .fields(Fields::new().i(Key::Code, 101).expect("code")),
        at(5),
    )
    .expect("closes");
    let c = log.fold().clause(now(6)).expect("clause");
    assert_eq!(
        (c.words.as_str(), c.tone),
        ("failed · exit 101", Tone::Failed)
    );
    log.mark(
        Mark::new(Kind::Seen, Src::Ctl, Conf::Exact).by(Actor::Human),
        at(7),
    )
    .expect("seen");
    assert!(log.state().top_need().is_none(), "`seen` clears a failure");
}

/// THE FAILURE-SUPERSEDE RULE ([`aterm_slog::lifts_failure`]): a shell
/// command's failure is the newest command's outcome, as `status outcome=`
/// holds it. A later `cmd` run that ends `ok` lifts it; one that ends `fail`
/// replaces it with its own (one need, the newest's age, never one per failed
/// command). NEGATIVE CONTROLS: a later run that ended `unknown`, and a run of
/// another kind ending `ok`, leave the failure standing.
#[test]
fn a_later_command_supersedes_an_earlier_commands_failure() {
    let mut log = Log::new(0);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("session");
    let cmd = |log: &mut Log, kind: Kind, out: RunOut, ms: u64| {
        let r = log
            .open(Open::new(kind, Src::Shell, Conf::Strong), at(ms))
            .expect("open");
        log.close(
            r,
            Close::new(Out::Run(out), Src::Shell, Conf::Exact),
            at(ms + 1),
        )
        .expect("close");
        r
    };
    let failures = |log: &Log| -> Vec<(String, u64)> {
        log.state()
            .needs
            .iter()
            .flatten()
            .filter(|n| n.word == "failure")
            .map(|n| (n.node.to_string(), n.t))
            .collect()
    };
    let grep = cmd(&mut log, Kind::Cmd, RunOut::Fail, 10);
    assert_eq!(failures(&log), vec![(grep.to_string(), WALL0 + 11)]);
    // NEGATIVE CONTROLS: an interrupted command and a program in front.
    cmd(&mut log, Kind::Cmd, RunOut::Unknown, 20);
    cmd(&mut log, Kind::Prog, RunOut::Ok, 30);
    assert_eq!(
        failures(&log),
        vec![(grep.to_string(), WALL0 + 11)],
        "an unknown end and another kind's success say nothing about it"
    );
    // A later failure replaces it: one need, dated at the newest.
    let make = cmd(&mut log, Kind::Cmd, RunOut::Fail, 40);
    assert_eq!(failures(&log), vec![(make.to_string(), WALL0 + 41)]);
    // A later success lifts it, and brief stops saying `need=failure`.
    cmd(&mut log, Kind::Cmd, RunOut::Ok, 50);
    assert_eq!(failures(&log), Vec::<(String, u64)>::new());
    assert!(log.state().top_need().is_none());
    let b = log.brief("s-1", 600, WALL0 + 600_000);
    assert!(!b.text.contains("failure"), "{}", b.text);
}

/// A closed, held turn and its log: the screen's `done` close at `strong`.
fn closed_turn() -> (Log, aterm_slog::Node) {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    log.close(
        turn,
        Close::new(
            Out::Turn(aterm_slog::TurnOut::Done),
            Src::Screen,
            Conf::Strong,
        ),
        at(5),
    )
    .expect("closes");
    (log, turn)
}

fn amend_out(log: &mut Log, node: aterm_slog::Node, kind: Kind, out: &str, conf: Conf, ms: u64) {
    log.amend(
        node,
        Amend::new(
            kind,
            Src::Transcript,
            conf,
            Fields::new().word(Key::Out, out).expect("out"),
        ),
        at(ms),
    )
    .expect("amends");
}

/// PHASE 3B: an EXACT amend refines a held turn's outcome
/// and moves it between the totals' buckets, the bus totals following; a
/// STRONG one (an unmeasured build's transcript) never overrules the
/// screen's close, whatever its seq.
#[test]
fn an_exact_amend_refines_a_held_outcome_and_a_strong_one_never_does() {
    let (mut log, turn) = closed_turn();
    let t0 = log.state().totals.clone();
    assert_eq!((t0.done, t0.walled), (1, 0));
    // Strong, later seq: kept as a record, never the outcome.
    amend_out(
        &mut log,
        turn,
        Kind::Turn,
        "wall:usage-session",
        Conf::Strong,
        6,
    );
    assert_eq!(log.state().last[1].as_ref().map(|c| c.out), Some("done"));
    assert_eq!(log.state().totals, t0, "a strong amend overruled the close");
    assert_eq!(
        log.state().last[1]
            .as_ref()
            .and_then(|c| c.f.word(Key::Out)),
        Some("done"),
        "the held field agrees with the outcome"
    );
    // Exact: the outcome is refined and re-bucketed.
    amend_out(
        &mut log,
        turn,
        Kind::Turn,
        "wall:usage-session",
        Conf::Exact,
        7,
    );
    let st = log.state();
    assert_eq!(
        st.last[1].as_ref().map(|c| c.out),
        Some("wall:usage-session")
    );
    assert_eq!(
        (st.totals.done, st.totals.walled, st.totals.turns),
        (0, 1, 1)
    );
    let bus = log.fold().bus_totals();
    assert_eq!((bus.done, bus.walled), (0, 1));
    assert_eq!(bus.last_turn.map(|(_, o)| o), Some("wall:usage-session"));
    // A later strong amend back to `done` changes nothing: exact holds.
    amend_out(&mut log, turn, Kind::Turn, "done", Conf::Strong, 8);
    assert_eq!(
        log.state().last[1].as_ref().map(|c| c.out),
        Some("wall:usage-session")
    );
    // The same fold from the records, applied twice, agrees (idempotent).
    let mut f = Fold::new(0);
    for r in log.window() {
        f.apply(r);
        f.apply(r);
    }
    assert_eq!(f.state(), log.state());
}

/// PHASE 3B (S9, A5; found by `SessionLogFold`'s `ConfidenceMonotone`): an
/// exact `out=` amend made while the turn is still open outranks the
/// screen's strong close that follows it — the close counts the turn in the
/// exact word's bucket and its held field says the same — while a strong
/// amend while open never overrules even a weaker (heuristic) close, and
/// `brief` prints what the fold holds.
#[test]
fn an_exact_amend_while_open_outranks_the_later_screen_close() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let open_turn = |log: &mut Log, ms: u64| {
        log.open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(ms),
        )
        .expect("turn")
    };
    let turn = open_turn(&mut log, 1);
    amend_out(
        &mut log,
        turn,
        Kind::Turn,
        "wall:usage-session",
        Conf::Exact,
        2,
    );
    log.close(
        turn,
        Close::new(
            Out::Turn(aterm_slog::TurnOut::Done),
            Src::Screen,
            Conf::Strong,
        ),
        at(3),
    )
    .expect("closes");
    let held = log.state().last[1].clone().expect("held");
    assert_eq!(held.out, "wall:usage-session");
    assert_eq!(held.f.word(Key::Out), Some(held.out), "field and outcome");
    let t = &log.state().totals;
    assert_eq!((t.done, t.walled), (0, 1));
    let b = log.brief("s-1", aterm_slog::DEFAULT_BUDGET, WALL0 + 4);
    assert!(
        b.text
            .lines()
            .any(|l| l.starts_with("t1 wall:usage-session")),
        "{}",
        b.text
    );

    let second = open_turn(&mut log, 5);
    amend_out(
        &mut log,
        second,
        Kind::Turn,
        "wall:context",
        Conf::Strong,
        6,
    );
    log.close(
        second,
        Close::new(
            Out::Turn(aterm_slog::TurnOut::Asked),
            Src::Screen,
            Conf::Heuristic,
        ),
        at(7),
    )
    .expect("closes");
    let held = log.state().last[1].clone().expect("held");
    assert_eq!(held.out, "asked", "a strong amend overruled the close");
    assert_eq!(held.f.word(Key::Out), Some("asked"), "field and outcome");
    let t = &log.state().totals;
    assert_eq!((t.asked, t.walled), (1, 1));
}

/// A turn no longer held (a newer one closed since) keeps its totals: the
/// stated approximation (plan §11 Q9); its own record still says the
/// refinement, which `brief` reads.
#[test]
fn an_exact_amend_of_an_unheld_turn_keeps_the_totals() {
    let (mut log, first) = closed_turn();
    let run = log.state().run().expect("run").node;
    let second = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(10),
        )
        .expect("turn");
    log.close(
        second,
        Close::new(
            Out::Turn(aterm_slog::TurnOut::Done),
            Src::Screen,
            Conf::Strong,
        ),
        at(12),
    )
    .expect("closes");
    let t0 = log.state().totals.clone();
    amend_out(&mut log, first, Kind::Turn, "wall:context", Conf::Exact, 13);
    assert_eq!(log.state().totals, t0);
}

/// PHASE 3B (A5): an exact `confirmed=1` raises the wall step's need to the
/// transcript's source and confidence; a strong one, or an exact amend
/// without `confirmed=1`, leaves the need as the screen raised it.
#[test]
fn an_exact_confirmation_raises_the_step_needs_confidence() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.observe(&mut log, Verdict::Busy, None, at(1));
    tr.observe(
        &mut log,
        Verdict::Wall(WallKind::UsageSession),
        Some("3pm"),
        at(2),
    );
    let step = log.state().step().expect("the wall step").node;
    let need = |log: &Log| {
        let n = log.state().top_need().expect("the wall's need").clone();
        (n.word, n.src, n.conf)
    };
    assert_eq!(
        need(&log),
        ("wall:usage-session", Src::Screen, Conf::Strong)
    );
    let confirm = |log: &mut Log, conf: Conf, confirmed: u64, ms: u64| {
        log.amend(
            step,
            Amend::new(
                Kind::Wall,
                Src::Transcript,
                conf,
                Fields::new()
                    .u(Key::Confirmed, confirmed)
                    .expect("confirmed"),
            ),
            at(ms),
        )
        .expect("amends");
    };
    confirm(&mut log, Conf::Strong, 1, 3);
    assert_eq!(
        need(&log).2,
        Conf::Strong,
        "a strong confirmation raised it"
    );
    confirm(&mut log, Conf::Exact, 0, 4);
    assert_eq!(need(&log).2, Conf::Strong, "no confirmation raised it");
    confirm(&mut log, Conf::Exact, 1, 5);
    assert_eq!(
        need(&log),
        ("wall:usage-session", Src::Transcript, Conf::Exact)
    );
    let b = log.brief("s-1", 2000, WALL0 + 6);
    assert!(b.text.contains("[transcript!]"), "{}", b.text);
}

/// PHASE 3B (A5): the clause names the row it came from, keeps its head
/// words without the coarse age, and `clause_of_proven` admits only a
/// proven author's label: a `ctl` label neither shows nor hides the step
/// beneath it there, while `clause_of` (brief's) still shows it.
#[test]
fn the_clause_names_its_row_and_the_proven_clause_skips_a_ctl_label() {
    use aterm_slog::{ClauseRow, clause_of, clause_of_proven};
    let now = |ms: u64| WALL0 + ms;
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let row = |log: &Log, ms: u64| {
        let c = clause_of(log.state(), now(ms)).expect("a clause");
        (
            c.row,
            c.words.as_str().to_string(),
            c.head.as_str().to_string(),
        )
    };
    assert_eq!(row(&log, 1).0, ClauseRow::Run);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    assert_eq!(
        row(&log, 120_001),
        (ClauseRow::Turn, "working · 2m".into(), "working".into())
    );
    log.open(
        Open::new(Kind::Tool, Src::Transcript, Conf::Exact)
            .under(turn)
            .fields(Fields::new().word(Key::Cls, "shell").expect("cls")),
        at(2),
    )
    .expect("tool");
    assert_eq!(
        row(&log, 60_002),
        (
            ClauseRow::Step,
            "running shell · 1m".into(),
            "running shell".into()
        )
    );
    let label = |log: &mut Log, by: Actor, text: &str, ms: u64| {
        log.mark(
            Mark::new(Kind::Label, Src::Mark, Conf::Claim)
                .by(by)
                .fields(Fields::new().text(Key::Text, text).expect("text")),
            at(ms),
        )
        .expect("label");
    };
    label(&mut log, Actor::Ctl, "ctl says hi", 3);
    let any = clause_of(log.state(), now(4)).expect("a clause");
    assert_eq!(
        (any.row, any.head.as_str()),
        (ClauseRow::Label, "ctl says hi")
    );
    let proven = clause_of_proven(log.state(), now(4)).expect("a clause");
    assert_eq!(
        (proven.row, proven.head.as_str()),
        (ClauseRow::Step, "running shell"),
        "a ctl label neither shows nor hides the step in the proven clause"
    );
    label(&mut log, Actor::Harness, "reviewing", 5);
    let proven = clause_of_proven(log.state(), now(6)).expect("a clause");
    assert_eq!(
        (proven.row, proven.head.as_str(), proven.by),
        (ClauseRow::Label, "reviewing", Actor::Harness)
    );
    // A need outranks every label in both.
    log.open(
        Open::new(Kind::Approval, Src::Screen, Conf::Strong)
            .under(turn)
            .fields(Fields::new().word(Key::Box, "edit").expect("box")),
        at(7),
    )
    .expect("box");
    for c in [
        clause_of(log.state(), now(8)),
        clause_of_proven(log.state(), now(8)),
    ] {
        let c = c.expect("a clause");
        assert_eq!(
            (c.row, c.words.as_str(), c.head.as_str()),
            (ClauseRow::Need, "approval · edit", "approval · edit")
        );
    }
}

/// ONLY A MOVE CLOSES MAIL (phase 6, 6F-6): every `inbox seen` is a
/// `mail-seen` mark, `moved=1` when it moved the watermark or decided a
/// verdict and `moved=0` when it moved nothing. Two mails in and one seen
/// that moved leave one open, and that seen repeated three times leaves the
/// fold's mail counts where they were; a mark with no `moved=` (an older
/// writer's, written only on a move) closes one. NEGATIVE CONTROL (by
/// scratch revert): a fold that reads the unmoved marks closes the second
/// mail on the first repeat.
#[test]
fn a_repeated_inbox_seen_moves_no_mail_count() {
    let mut log = Log::new(0);
    let mark = |log: &mut Log, k: Kind, f: Fields, ms: u64| {
        log.mark(Mark::new(k, Src::Fabric, Conf::Exact).fields(f), at(ms))
            .expect("a mail mark");
    };
    for (inbox, ms) in [(1, 1), (2, 2)] {
        let f = Fields::new()
            .word(Key::Dir, "in")
            .and_then(|f| f.u(Key::Inbox, inbox))
            .expect("dir");
        mark(&mut log, Kind::Mail, f, ms);
    }
    let seen = |moved: Option<u64>| {
        let f = Fields::new()
            .u(Key::Inbox, 1)
            .and_then(|f| f.u(Key::Seen, 1))
            .expect("seen");
        match moved {
            Some(m) => f.u(Key::Moved, m).expect("moved"),
            None => f,
        }
    };
    mark(&mut log, Kind::MailSeen, seen(Some(1)), 3);
    let counts = |log: &Log| (log.state().mail_open, log.state().totals.mail_in);
    assert_eq!(counts(&log), (1, 2));
    for ms in 4..7 {
        mark(&mut log, Kind::MailSeen, seen(Some(0)), ms);
        assert_eq!(counts(&log), (1, 2), "a repeat at {ms} closed mail");
    }
    mark(&mut log, Kind::MailSeen, seen(None), 7);
    assert_eq!(counts(&log), (0, 2), "an older writer's seen is a move");
}
