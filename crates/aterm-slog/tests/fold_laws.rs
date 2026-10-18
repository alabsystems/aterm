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
