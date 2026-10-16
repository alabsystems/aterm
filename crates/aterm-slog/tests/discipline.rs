// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The open/close discipline of §2.3, enforced by the API.

use aterm_slog::{
    Actor, Close, Conf, FieldError, Fields, Key, Kind, Limits, Log, Lvl, Mark, Node, Op, Open, Out,
    Rec, Refused, RunOut, SessionOut, ShortId, Src, StepOut, Tier, TurnOut, Val, encode,
    encode_bus,
};
use aterm_spec::rng::XorShift;

use crate::support::{agent_session, at, lines};

fn open(log: &mut Log, kind: Kind, under: Option<Node>, ms: u64) -> Result<Node, Refused> {
    let mut o = Open::new(kind, Src::Screen, Conf::Strong);
    o.under = under;
    log.open(o, at(ms))
}

fn done(log: &mut Log, node: Node, ms: u64) -> Result<u64, Refused> {
    let out = match node.lvl {
        Lvl::Run => Out::Run(RunOut::Ok),
        Lvl::Turn => Out::Turn(TurnOut::Done),
        Lvl::Step => Out::Step(StepOut::Passed),
        _ => Out::Session(SessionOut::ShellExit),
    };
    log.close(node, Close::new(out, Src::Screen, Conf::Strong), at(ms))
}

#[test]
fn closing_a_node_closes_its_children_innermost_first_gone() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = open(&mut log, Kind::Turn, Some(run), 10).expect("turn");
    let tool = open(&mut log, Kind::Tool, Some(turn), 20).expect("tool");
    let appr = open(&mut log, Kind::Approval, Some(tool), 30).expect("approval");
    assert_eq!(
        log.open_entry(appr).map(|e| (e.parent, e.orphan)),
        Some((tool, false)),
        "the approval nests under its tool step"
    );
    done(&mut log, run, 1000).expect("the run closes");
    let closes: Vec<&Rec> = log.window().filter(|r| r.op == Op::Close).collect();
    let order: Vec<(Node, &str)> = closes
        .iter()
        .map(|r| (r.node, r.out().unwrap_or("?")))
        .collect();
    assert_eq!(
        order,
        vec![(appr, "gone"), (tool, "gone"), (turn, "gone"), (run, "ok")]
    );
    let run_close = closes.last().expect("run");
    assert_eq!(run_close.f.get_u(Key::DurMs), Some(1000));
    assert_eq!(run_close.f.get_u(Key::Steps), Some(2), "the run's rollup");
    assert_eq!(run_close.f.get_u(Key::Appr), Some(1));
    assert!(log.open_nodes().is_empty());
}

#[test]
fn a_node_closes_exactly_once() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = open(&mut log, Kind::Turn, Some(run), 1).expect("turn");
    done(&mut log, turn, 2).expect("closes");
    let before = lines(&log);
    assert_eq!(done(&mut log, turn, 3), Err(Refused::NotOpen(turn)));
    assert_eq!(lines(&log), before, "a refused close writes nothing");
    assert_eq!(
        log.close(
            run,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(4)
        ),
        Err(Refused::OutDoesNotFit(run))
    );
}

#[test]
fn a_new_turn_supersedes_an_unclosed_one() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let t1 = open(&mut log, Kind::Turn, Some(run), 1).expect("t1");
    let p = open(&mut log, Kind::Question, Some(t1), 2).expect("step");
    let t2 = open(&mut log, Kind::Turn, Some(run), 3).expect("t2");
    let outs: Vec<(Node, &str)> = log
        .window()
        .filter(|r| r.op == Op::Close)
        .map(|r| (r.node, r.out().unwrap_or("?")))
        .collect();
    assert_eq!(outs, vec![(p, "gone"), (t1, "superseded")]);
    assert_eq!(log.open_under(run, Lvl::Turn).count(), 1);
    assert!(log.is_open(t2));
}

#[test]
fn an_orphan_attaches_to_the_nearest_open_ancestor() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = open(&mut log, Kind::Turn, Some(run), 1).expect("turn");
    done(&mut log, turn, 2).expect("closed");
    // A step asked under the closed turn hangs from the run, marked.
    let p = open(&mut log, Kind::Wall, Some(turn), 3).expect("step");
    let r = log.window().last().expect("its open").clone();
    assert_eq!((r.node, r.par), (p, Some(run)));
    assert_eq!(r.f.get_u(Key::Orphan), Some(1));
    // A turn with no run anywhere hangs from the session, marked; a parent
    // is never invented.
    let mut bare = Log::new(0);
    let t = open(&mut bare, Kind::Turn, None, 0).expect("turn");
    let r = bare.window().last().expect("its open");
    assert_eq!((r.node, r.par), (t, Some(Node::SESSION)));
    assert_eq!(r.f.get_u(Key::Orphan), Some(1));
    assert_eq!(bare.open_nodes().len(), 1);
}

#[test]
fn node_ids_count_per_level_and_are_never_reused() {
    let mut log = Log::with_limits(0, Limits::new(4, 1 << 20).expect("tiny"));
    let run = agent_session(&mut log, 0);
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..50u64 {
        let t = open(&mut log, Kind::Turn, Some(run), i * 10).expect("turn");
        let p = open(&mut log, Kind::Approval, Some(t), i * 10 + 1).expect("step");
        assert!(seen.insert(t) && seen.insert(p), "an id came back");
        done(&mut log, t, i * 10 + 2).expect("closes");
    }
    assert_eq!(log.deepest_open(Lvl::Run), Some(run));
    assert!(log.issued(Node::new(Lvl::Turn, 50)));
    assert!(!log.issued(Node::new(Lvl::Turn, 51)));
}

#[test]
fn closing_the_session_closes_everything_and_refuses_more() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let _turn = open(&mut log, Kind::Turn, Some(run), 1).expect("turn");
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact).by(Actor::Human),
        at(9),
    )
    .expect("closes");
    let last = log.window().last().expect("the session's close");
    assert_eq!(
        encode(last, Tier::Wire).expect("fits"),
        format!(
            "{} close - spawn out=ui-close dur_ms=9 by=human src=aterm conf=exact t={}",
            last.seq,
            crate::support::WALL0 + 9
        )
    );
    assert!(log.open_nodes().is_empty());
    assert_eq!(log.state().top_need().map(|n| n.word), Some("exited"));
    assert_eq!(
        open(&mut log, Kind::Turn, Some(run), 10),
        Err(Refused::SessionClosed)
    );
    assert_eq!(
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(11)),
        Err(Refused::SessionClosed)
    );
}

#[test]
fn refusals_change_nothing() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let before = lines(&log);
    let seq = log.last_seq();
    assert!(matches!(
        open(&mut log, Kind::Bell, Some(run), 1),
        Err(Refused::KindDoesNotFit(Kind::Bell))
    ));
    let f = Fields::new()
        .text(Key::Cwd, &"/x".repeat(200))
        .and_then(|f| f.text(Key::Title, &"t".repeat(300)))
        .expect("each fits");
    assert!(matches!(
        log.mark(
            Mark::new(Kind::Cwd, Src::Shell, Conf::Exact).fields(f),
            at(2)
        ),
        Err(Refused::Encode(_))
    ));
    assert_eq!((lines(&log), log.last_seq()), (before, seq));
}

/// Random opens and closes: every open's parent is open when it opens,
/// every node closes at most once, and seqs strictly increase.
#[test]
fn random_traffic_keeps_the_tree_well_formed() {
    for seed in 1..=20u64 {
        let mut rng = XorShift::new(seed);
        let mut log = Log::with_limits(0, Limits::new(4096, 1 << 22).expect("roomy"));
        let _ = agent_session(&mut log, 0);
        let mut open_now: std::collections::BTreeSet<Node> =
            log.open_nodes().iter().map(|e| e.node).collect();
        let mut closed = std::collections::BTreeSet::new();
        let mut last_seq = log.last_seq();
        let kinds = [
            Kind::Cmd,
            Kind::Agent,
            Kind::Turn,
            Kind::Approval,
            Kind::Tool,
        ];
        for i in 0..400u64 {
            let ms = 10 + i;
            if rng.below(2) == 0 || log.open_nodes().is_empty() {
                let kind = kinds[rng.index(kinds.len())];
                let under = log
                    .open_nodes()
                    .get(rng.index(log.open_nodes().len().max(1)))
                    .map(|e| e.node);
                let _ = open(&mut log, kind, under, ms);
            } else {
                let nodes = log.open_nodes();
                let n = nodes[rng.index(nodes.len())].node;
                done(&mut log, n, ms).expect("an open node closes");
            }
            let fresh: Vec<Rec> = log.window().filter(|r| r.seq > last_seq).cloned().collect();
            for r in fresh {
                assert!(r.seq > last_seq, "seq went backwards");
                last_seq = r.seq;
                match r.op {
                    Op::Open => {
                        let par = r.par.expect("an open names its parent");
                        assert!(
                            par == Node::SESSION || open_now.contains(&par),
                            "{r:?} opened under a closed parent"
                        );
                        open_now.insert(r.node);
                    }
                    Op::Close => {
                        assert!(open_now.remove(&r.node), "{r:?} closed twice or unopened");
                        assert!(closed.insert(r.node));
                        assert!(
                            !log.open_nodes().iter().any(|e| e.parent == r.node),
                            "a parent closed under an open child"
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}

/// A named parent of the child's own level is natural where §2.1 and §2.7
/// say so: an agent run started from zsh hangs from zsh's `cmd` run, and an
/// approval from its open `tool` step, neither an orphan; closing the
/// parent cascades to them.
#[test]
fn a_same_level_parent_is_natural() {
    let mut log = Log::new(0);
    let cmd = open(&mut log, Kind::Cmd, None, 0).expect("cmd");
    let agent = open(&mut log, Kind::Agent, Some(cmd), 1).expect("agent");
    let r = log.window().last().expect("its open").clone();
    assert_eq!((r.node, r.par), (agent, Some(cmd)));
    assert_eq!(r.f.get_u(Key::Orphan), None, "{r:?}");
    let turn = open(&mut log, Kind::Turn, Some(agent), 2).expect("turn");
    let tool = open(&mut log, Kind::Tool, Some(turn), 3).expect("tool");
    let appr = open(&mut log, Kind::Approval, Some(tool), 4).expect("approval");
    let r = log.window().last().expect("its open").clone();
    assert_eq!((r.node, r.par), (appr, Some(tool)));
    assert_eq!(r.f.get_u(Key::Orphan), None, "{r:?}");
    assert_eq!(log.open_entry(appr).map(|e| e.parent), Some(tool));

    // OSC 133 D closes the cmd run: everything under it goes, innermost
    // first.
    done(&mut log, cmd, 10).expect("the cmd closes");
    let outs: Vec<(Node, &str)> = log
        .window()
        .filter(|r| r.op == Op::Close)
        .map(|r| (r.node, r.out().unwrap_or("?")))
        .collect();
    assert_eq!(
        outs,
        vec![
            (appr, "gone"),
            (tool, "gone"),
            (turn, "gone"),
            (agent, "gone"),
            (cmd, "ok")
        ]
    );
    // A step asked under a closed step whose parent step is open hangs from
    // that step, an orphan.
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let turn = open(&mut log, Kind::Turn, Some(run), 1).expect("turn");
    let outer = open(&mut log, Kind::Tool, Some(turn), 2).expect("outer");
    let inner = open(&mut log, Kind::Subagent, Some(outer), 3).expect("inner");
    done(&mut log, inner, 4).expect("inner closes");
    let late = open(&mut log, Kind::Approval, Some(inner), 5).expect("late");
    let r = log.window().last().expect("its open").clone();
    assert_eq!(
        (r.node, r.par, r.f.get_u(Key::Orphan)),
        (late, Some(outer), Some(1))
    );
}

/// The log writes the outcome, the duration, the rollup and `orphan=`: a
/// caller's fields carrying one are refused and write nothing, and a
/// duration measured elsewhere goes through the typed builders.
#[test]
fn the_log_owns_its_own_keys() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let before = lines(&log);
    for f in [
        Fields::new().word(Key::Out, "approved"),
        Fields::new().u(Key::DurMs, 1),
        Fields::new().word(Key::Dur, "wall"),
        Fields::new().u(Key::Steps, 0),
        Fields::new().tok(Key::Tools, "shell:1"),
    ] {
        let f = f.expect("a field");
        let key = f.iter().next().expect("one").0;
        assert_eq!(
            log.close(
                run,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact).fields(f),
                at(5)
            ),
            Err(Refused::Field(FieldError::Shape(key)))
        );
    }
    assert_eq!(
        log.open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(run)
                .fields(Fields::new().u(Key::Orphan, 1).expect("u")),
            at(6)
        ),
        Err(Refused::Field(FieldError::Shape(Key::Orphan)))
    );
    assert_eq!(lines(&log), before, "refusals write nothing");

    // A block the sweep saw only complete: its wall stamps give `t=` and
    // the duration.
    let cmd = open(&mut log, Kind::Cmd, None, 7).expect("cmd");
    log.close(
        cmd,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact)
            .dur_wall(4_321)
            .fields(Fields::new().i(Key::Code, 2).expect("code")),
        at(8),
    )
    .expect("closes");
    let line = encode(log.window().last().expect("its close"), Tier::Wire).expect("fits");
    assert!(
        line.contains(" out=fail dur_ms=4321 dur=wall code=2"),
        "{line}"
    );
    let turn = open(&mut log, Kind::Turn, Some(run), 9).expect("turn");
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong).dur_ms(77),
        at(5_000),
    )
    .expect("closes");
    let last = log.window().last().expect("its close");
    assert_eq!(
        (last.f.get_u(Key::DurMs), last.f.get_word(Key::Dur)),
        (Some(77), None)
    );
}

/// A successor continues the predecessor's node ids (§2.2): `t3` names one
/// turn for as long as the session lives, across an update that carries no
/// records.
#[test]
fn node_ids_continue_across_a_handoff() {
    let mut old = Log::new(1);
    let run = agent_session(&mut old, 0);
    for i in 0..3 {
        let t = open(&mut old, Kind::Turn, Some(run), 1 + i).expect("turn");
        open(&mut old, Kind::Question, Some(t), 2 + i).expect("step");
    }
    let ids = old.id_counters();
    assert_eq!((ids.run, ids.turn, ids.step), (1, 3, 3));
    let mut new = Log::after_handoff(2, Limits::SESSION, ids, at(100));
    let run2 = open(&mut new, Kind::Agent, None, 101).expect("run");
    let t = open(&mut new, Kind::Turn, Some(run2), 102).expect("turn");
    let p = open(&mut new, Kind::Approval, Some(t), 103).expect("step");
    assert_eq!(
        (run2, t, p),
        (
            Node::new(Lvl::Run, 2),
            Node::new(Lvl::Turn, 4),
            Node::new(Lvl::Step, 4)
        )
    );
    assert!(
        new.issued(Node::new(Lvl::Turn, 3)),
        "the old turn is still a known id"
    );
}

/// Every record the log accepts fits both forms: the line, and for the turn
/// class the bus mirror carries, the `v=2` body at its widest header. A
/// record whose line fits but whose body would not is refused, writing
/// nothing.
#[test]
fn every_accepted_turn_class_record_fits_the_bus() {
    let id = |i: u64| ShortId::new(&format!("{i:0>39}")).expect("an id");
    let sid = Actor::Sid(ShortId::new(&"s".repeat(40)).expect("a sid"));
    let ids = [
        Key::Gen,
        Key::Fp,
        Key::Cont,
        Key::Rule,
        Key::Prev,
        Key::Lineage,
    ];
    let counts = [Key::Ledger, Key::N, Key::Rev, Key::Epoch, Key::Pct];
    let toks = [
        Key::Prog,
        Key::Detail,
        Key::Model,
        Key::Ver,
        Key::Agent,
        Key::Effort,
    ];
    let mut refused = 0;
    let mut accepted = 0;
    for seed in 1..=400u64 {
        let mut rng = XorShift::new(seed);
        let mut log = Log::new(u32::MAX);
        let run = agent_session(&mut log, 0);
        let mut f = Fields::new();
        for _ in 0..12 {
            let (key, val) = match rng.below(3) {
                0 => (ids[rng.index(ids.len())], Val::Id(id(rng.below(1 << 40)))),
                1 => (counts[rng.index(counts.len())], Val::U(u64::MAX)),
                _ => {
                    let key = toks[rng.index(toks.len())];
                    let t = aterm_slog::BoundedToken::new(key, &"Ab9".repeat(20)).expect("a token");
                    (key, Val::Tok(t))
                }
            };
            let _ = f.set(key, val);
        }
        let kind = if rng.below(2) == 0 {
            Kind::Turn
        } else {
            Kind::Cmd
        };
        let mut o = Open::new(kind, Src::Screen, Conf::Strong).by(sid).fields(f);
        o.under = Some(run);
        let before = log.last_seq();
        match log.open(o, at(1)) {
            Ok(_) => accepted += 1,
            Err(Refused::Encode(_)) => {
                refused += 1;
                assert_eq!(log.last_seq(), before, "a refusal wrote something");
            }
            Err(other) => panic!("{other:?}"),
        }
        for r in log.window().filter(|r| r.is_turn_class()) {
            let mut wide = r.clone();
            wide.seq = u64::MAX;
            assert!(
                encode_bus(&wide, Tier::WireMeta, u32::MAX, Some(u64::MAX)).is_ok(),
                "accepted but not mirrorable: {r:?}"
            );
        }
    }
    assert!(
        accepted > 0 && refused > 0,
        "{accepted} accepted, {refused} refused"
    );
}
