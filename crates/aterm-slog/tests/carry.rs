// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless-update carry tests for `aterm-slog`.

use aterm_slog::{
    Actor, BoxWord, CarryAdopt, CarryBudget, CarryError, Close, Conf, Cursor, Fields, Fold, GapWhy,
    IdCounters, Key, Kind, Limits, Log, Mark, Node, Open, Out, Rec, RunOut, SessionOut, SnapError,
    Src, Stamp, StepOut, Tier, TurnOut, TurnTracker, Verdict, WallKind, encode,
};
use aterm_spec::rng::XorShift;

use crate::support::{WALL0, agent_session, at};

fn traffic(log: &mut Log, rng: &mut XorShift, n: usize, ms0: u64) -> Node {
    let run = log
        .open_nodes()
        .first()
        .map(|e| e.node)
        .expect("an agent run");
    let mut ms = ms0;
    let mut turn: Option<Node> = log
        .open_nodes()
        .iter()
        .find(|e| e.node.lvl == aterm_slog::Lvl::Turn)
        .map(|e| e.node);
    for _ in 0..n {
        ms += 1 + rng.below(50);
        match rng.below(12) {
            0 if turn.is_none() => {
                turn = log
                    .open(
                        Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                            .under(run)
                            .by(Actor::Human),
                        at(ms),
                    )
                    .ok();
            }
            1 if turn.is_some() => {
                let _ = log.close(
                    turn.take().expect("open"),
                    Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                    at(ms),
                );
            }
            2 if turn.is_some() => {
                let f = Fields::new().word(Key::Box, "bash").expect("box");
                if let Ok(s) = log.open(
                    Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                        .under(turn.expect("open"))
                        .fields(f),
                    at(ms),
                ) && rng.below(2) == 0
                {
                    let _ = log.close(
                        s,
                        Close::new(Out::Step(StepOut::Approved), Src::Screen, Conf::Strong)
                            .by(Actor::Human),
                        at(ms + 1),
                    );
                }
            }
            3 => {
                let c = log
                    .open(
                        Open::new(Kind::Cmd, Src::Shell, Conf::Strong)
                            .fields(Fields::new().tok(Key::Detail, "cargo").expect("detail")),
                        at(ms),
                    )
                    .expect("cmd");
                let code = i64::from(rng.below(3) == 0);
                let out = if code == 0 { RunOut::Ok } else { RunOut::Fail };
                let _ = log.close(
                    c,
                    Close::new(Out::Run(out), Src::Shell, Conf::Exact)
                        .fields(Fields::new().i(Key::Code, code).expect("code")),
                    at(ms + 3),
                );
            }
            4 => {
                let _ = log.mark(
                    Mark::new(Kind::Need, Src::Mark, Conf::Claim)
                        .by(Actor::Harness)
                        .fields(Fields::new().word(Key::Word, "review").expect("word")),
                    at(ms),
                );
            }
            5 => {
                let _ = log.mark(
                    Mark::new(Kind::Label, Src::Mark, Conf::Claim)
                        .by(Actor::Harness)
                        .fields(
                            Fields::new()
                                .text(Key::Text, "fixing 2 of 5 ")
                                .expect("text"),
                        ),
                    at(ms),
                );
            }
            6 => {
                let _ = log.mark(
                    Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
                        .fields(Fields::new().word(Key::Dir, "in").expect("dir")),
                    at(ms),
                );
            }
            7 => {
                let _ = log.mark(
                    Mark::new(Kind::Status, Src::Status, Conf::Strong)
                        .fields(Fields::new().word(Key::Phase, "running").expect("phase")),
                    at(ms),
                );
            }
            _ => {
                let f = Fields::new()
                    .word(Key::Word, ["busy", "idle", "prompt"][rng.index(3)])
                    .expect("word");
                let _ = log.mark(
                    Mark::new(Kind::Agent, Src::Screen, Conf::Strong)
                        .at(run)
                        .fields(f),
                    at(ms),
                );
            }
        }
    }
    run
}

fn adopt_at(log: &Log, ms: u64) -> CarryAdopt {
    CarryAdopt {
        ep: log.ep() + 1,
        next_seq: log.last_seq() + 1000,
        ids: log.id_counters(),
        limits: Limits::SESSION,
        at: at(ms),
    }
}

#[test]
fn a_carry_of_a_legal_over_cap_open_path_reads_under_the_same_limits() {
    let limits = Limits::new(2, 1 << 20).expect("two records");
    let restore = |log: &Log| {
        let out = log
            .carry(CarryBudget::SESSION, &[])
            .expect("the carry fits");
        Log::from_carry(
            &out.lines,
            CarryAdopt {
                limits,
                ..adopt_at(log, 1_000)
            },
        )
    };
    let mut small = Log::new(1);
    for ms in 0..2 {
        small
            .mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
            .expect("a small mark");
    }
    assert!(restore(&small).is_ok(), "CONTROL: an ordinary carry fits");
    let mut excess = Log::new(1);
    for ms in 0..4 {
        excess
            .mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
            .expect("a mark");
    }
    assert!(
        restore(&excess).is_err(),
        "CONTROL: nonmandatory excess is refused"
    );

    let mut log = Log::with_limits(1, limits);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("the pinned turn opens");
    assert_eq!(log.len(), 3, "PRECONDITION: three legal pinned opens");
    let out = log.carry(CarryBudget::SESSION, &[]).expect("carry fits");
    let mut duplicate = out.lines.clone();
    let repeated = duplicate
        .iter()
        .find(|line| line.starts_with("r ") && line.contains(&format!(" open {run} ")))
        .expect("the run's open");
    let (_, record) = repeated
        .strip_prefix("r ")
        .unwrap()
        .split_once(' ')
        .unwrap();
    duplicate.push(format!("r {} {record}", log.last_seq() + 1));
    let lines = duplicate.len();
    duplicate[0] = duplicate[0]
        .split(' ')
        .map(|token| {
            if token.starts_with("last=") {
                format!("last={}", log.last_seq() + 1)
            } else if token.starts_with("carried=") {
                format!("carried={}", out.carried + 1)
            } else if token.starts_with("lines=") {
                format!("lines={lines}")
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        Log::from_carry(
            &duplicate,
            CarryAdopt {
                limits,
                ..adopt_at(&log, 1_000)
            }
        )
        .is_err(),
        "CONTROL: duplicate purported pinned opens are not mandatory excess"
    );
    let (back, _, _) = restore(&log).expect("a legal pinned open path must cross");
    assert!(back.is_open(run) && back.is_open(turn));
    let path = |log: &Log| {
        log.open_nodes()
            .iter()
            .map(|e| (e.node, e.kind, e.parent, e.t, e.orphan, e.roll))
            .collect::<Vec<_>>()
    };
    assert_eq!(path(&back), path(&log), "the same open path and rollups");

    let byte_limit = Limits::new(64, aterm_slog::MAX_RECORD_BYTES).expect("one largest record");
    let mut wide = Log::with_limits(1, byte_limit);
    wide.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
    let fields = || {
        Fields::new()
            .text(Key::Cwd, &"x".repeat(256))
            .expect("a Host field")
    };
    let r = wide
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(fields()),
            at(1),
        )
        .expect("the run opens");
    let t = wide
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(r)
                .fields(fields()),
            at(2),
        )
        .expect("the turn opens");
    assert!(
        wide.bytes() > byte_limit.bytes,
        "PRECONDITION: legal pinned bytes pass the cap"
    );
    let out = wide.carry(CarryBudget::SESSION, &[]).expect("carry fits");
    let (back, _, _) = Log::from_carry(
        &out.lines,
        CarryAdopt {
            limits: byte_limit,
            ..adopt_at(&wide, 1_000)
        },
    )
    .expect("a legal pinned byte excess must cross");
    assert!(back.is_open(r) && back.is_open(t));
}

#[test]
fn carry_and_closed_summary_exclude_content_fields_from_the_fold() {
    let mut leaked = Vec::new();
    for amend in [false, true] {
        let secret = if amend {
            "AMEND_ONLY_CONTENT"
        } else {
            "OPEN_ONLY_CONTENT"
        };
        let host = "/synthetic-host-field";
        let fields = || {
            Fields::new()
                .text(Key::Title, secret)
                .and_then(|f| f.text(Key::Cwd, host))
                .expect("bounded fields")
        };
        let mut log = Log::new(1);
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            at(0),
        )
        .expect("session opens");
        let mut open = Open::new(Kind::Agent, Src::Status, Conf::Strong);
        if !amend {
            open = open.fields(fields());
        }
        let run = log.open(open, at(1)).expect("run opens");
        if amend {
            log.amend(
                run,
                aterm_slog::Amend::new(Kind::Agent, Src::Transcript, Conf::Exact, fields()),
                at(2),
            )
            .expect("the accepted amend");
        }
        assert_eq!(log.state().run().unwrap().f.str(Key::Title), Some(secret));
        let out = log.carry(CarryBudget::SESSION, &[]).expect("carry fits");
        assert!(
            out.lines
                .iter()
                .filter(|l| l.starts_with("r "))
                .all(|l| !l.contains(secret)),
            "CONTROL: record encoding already excludes content"
        );
        let (back, _, _) = Log::from_carry(&out.lines, adopt_at(&log, 1_000)).expect("carry reads");
        assert_eq!(back.state().run().unwrap().f.str(Key::Cwd), Some(host));
        if out.lines.iter().any(|l| l.contains(secret)) {
            leaked.push(format!("{secret} in carry fold"));
        }

        log.close_session(
            Close::new(
                Out::Session(aterm_slog::SessionOut::UiClose),
                Src::Aterm,
                Conf::Exact,
            )
            .fields(
                Fields::new()
                    .text(Key::Title, "CLOSE_ONLY_CONTENT")
                    .and_then(|f| f.text(Key::Text, "host-close"))
                    .expect("close fields"),
            ),
            at(3),
        )
        .expect("session closes");
        let summary = log.closed_summary();
        let copy = Log::from_closed_summary(&summary).expect("summary reads");
        assert_eq!(
            copy.state().closed.as_ref().unwrap().f.str(Key::Text),
            Some("host-close")
        );
        for value in [secret, "CLOSE_ONLY_CONTENT"] {
            if summary.contains(value) {
                leaked.push(format!("{value} in closed summary"));
            }
        }
    }
    assert!(leaked.is_empty(), "Content fields escaped RAM: {leaked:?}");
}

#[test]
fn fold_snapshot_round_trips_and_folds_on_identically() {
    for seed in 1..=20u64 {
        let mut rng = XorShift::new(seed);
        let mut log = Log::new(1);
        agent_session(&mut log, 0);
        traffic(&mut log, &mut rng, 300, 10);
        let snap = log.fold().snapshot();
        let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
        let mut back = Fold::from_snapshot(&refs).expect("the snapshot reads");
        assert_eq!(
            back.state(),
            log.state(),
            "seed {seed}: the state as it stood"
        );
        assert_eq!(
            back.snapshot(),
            snap,
            "seed {seed}: and it writes the same lines"
        );
        let from = log.last_seq();
        traffic(&mut log, &mut rng, 200, 100_000);
        let later: Vec<&Rec> = log.window().filter(|r| r.seq > from).collect();
        for r in later {
            back.apply(r);
        }
        assert_eq!(back.state(), log.state(), "seed {seed}: folding on alike");
    }
    let mut rng = XorShift::new(1);
    let mut small = Log::with_limits(1, Limits::new(64, 1 << 20).expect("limits"));
    agent_session(&mut small, 0);
    traffic(&mut small, &mut rng, 400, 10);
    let mut rebuilt = Fold::new(1);
    for r in small.window() {
        rebuilt.apply(r);
    }
    assert!(small.lost() > 0);
    assert_ne!(
        rebuilt.state(),
        small.state(),
        "NEGATIVE CONTROL: the records alone are not the fold"
    );
}

#[test]
fn a_carried_log_reads_like_its_predecessor_and_says_the_handoff_hole() {
    let mut rng = XorShift::new(9);
    let mut log = Log::new(3);
    agent_session(&mut log, 0);
    traffic(&mut log, &mut rng, 400, 10);
    let cursor = Cursor::new(3, log.last_seq() - 5);
    let out = log
        .carry(CarryBudget::SESSION, &["x-host v=1 a=b".to_string()])
        .expect("carried");
    assert_eq!(out.shed, 0, "everything fits 128 KiB");
    assert!(out.bytes <= CarryBudget::SESSION.total);
    let a = adopt_at(&log, 900_000);
    let (new, host, stats) = Log::from_carry(&out.lines, a).expect("the carry reads");
    assert_eq!(host, vec!["x-host v=1 a=b".to_string()]);
    assert_eq!((stats.carried, stats.shed), (log.len() as u64, 0));
    assert_eq!(new.ep(), 4);
    assert_eq!(new.base_ep(), 3, "the same seq space");
    let old: Vec<String> = log
        .window()
        .map(|r| encode(r, Tier::Host).expect("fits"))
        .collect();
    let carried: Vec<String> = new
        .window()
        .take(log.len())
        .map(|r| encode(r, Tier::Host).expect("fits"))
        .collect();
    assert_eq!(carried, old, "the same records");
    assert_eq!(
        new.open_nodes()
            .iter()
            .map(|e| (e.node, e.roll))
            .collect::<Vec<_>>(),
        log.open_nodes()
            .iter()
            .map(|e| (e.node, e.roll))
            .collect::<Vec<_>>(),
        "the same open path and rollups"
    );
    assert_eq!(
        new.pinned_bytes(),
        log.pinned_bytes(),
        "the same records pinned"
    );
    let mark = new.window().last().expect("the handoff mark");
    assert_eq!(mark.seq, a.next_seq);
    assert_eq!(
        encode(mark, Tier::Host).expect("fits"),
        format!(
            "{} mark - handoff carried={} shed=0 epoch=4 by=aterm src=aterm conf=exact t={}",
            a.next_seq,
            log.len(),
            WALL0 + 900_000
        )
    );
    let mut st = log.state().clone();
    st.ep = 4;
    st.as_of = mark.seq;
    st.rev += 1;
    st.last_t = st.last_t.max(mark.t);
    assert_eq!(new.state(), &st, "the fold as it stood, at the next epoch");
    let read = new.read(cursor, usize::MAX);
    let g = read.gap.expect("the overlap is said");
    assert_eq!((g.why, g.lost), (GapWhy::Handoff, 0));
    assert_eq!(
        read.recs.len(),
        6,
        "the five after the cursor, then the mark"
    );
    assert_eq!(new.id_counters(), log.id_counters());

    let live_snapshot = log.carry_snapshot().prepare();
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact),
        at(900_001),
    )
    .expect("closed");
    assert!(
        log.carry(CarryBudget::SESSION, &[]).is_none(),
        "a closed session must not be revived by a carry"
    );
    assert!(
        log.carry_snapshot()
            .encode(CarryBudget::SESSION, &[])
            .is_none()
    );
    assert!(
        log.carry_snapshot()
            .prepare()
            .encode(CarryBudget::SESSION, &[])
            .is_none()
    );
    assert!(log.carry_snapshot().prepare().skeleton(&[]).is_none());
    let frozen = live_snapshot
        .encode(CarryBudget::SESSION, &[])
        .expect("the earlier live snapshot still crosses");
    let (continued, _, _) = Log::from_carry(&frozen.lines, a).expect("the live snapshot reads");
    assert!(continued.is_open(Node::SESSION));
}

#[test]
fn the_carry_cut_is_the_eviction_order_and_counts_what_it_shed() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut ms = 10;
    for _ in 0..40 {
        ms += 10;
        let t = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(ms),
            )
            .expect("turn");
        for _ in 0..3 {
            ms += 1;
            log.mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(t),
                at(ms),
            )
            .expect("detail");
        }
        log.close(
            t,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(ms),
        )
        .expect("closes");
    }
    let open_turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(ms + 1),
        )
        .expect("the open turn");
    let all = log.carry(CarryBudget::SESSION, &[]).expect("carried");
    assert_eq!(all.shed, 0, "NEGATIVE CONTROL: room for everything");
    let skeleton = log
        .carry_snapshot()
        .prepare()
        .skeleton(&[])
        .expect("skeleton fits");
    assert_eq!(skeleton.carried, 3, "only session, run and live turn opens");
    assert_eq!(skeleton.carried + skeleton.shed, all.carried);
    assert!(skeleton.bytes <= CarryBudget::SESSION.skeleton);
    assert!(Log::from_carry(&skeleton.lines, adopt_at(&log, 99_000)).is_ok());
    let tight = CarryBudget {
        total: all.skeleton_bytes + 3000,
        ..CarryBudget::SESSION
    };
    let cut = log.carry(tight, &[]).expect("carried");
    assert!(
        cut.shed > 0 && cut.bytes <= tight.total,
        "{} {}",
        cut.shed,
        cut.bytes
    );
    let (new, _, stats) = Log::from_carry(&cut.lines, adopt_at(&log, 99_000)).expect("reads");
    assert_eq!(stats.shed, cut.shed);
    assert_eq!(new.lost(), log.lost() + cut.shed, "shed counts as lost");
    let kept = |pred: &dyn Fn(&Rec) -> bool| new.window().filter(|r| pred(r)).count();
    let bells = kept(&|r| r.k == Kind::Bell);
    let opens = kept(&|r| r.op == aterm_slog::Op::Open && r.k == Kind::Turn && r.node != open_turn);
    let closes = kept(&|r| r.op == aterm_slog::Op::Close && r.k == Kind::Turn);
    assert!(
        bells == 0 || opens == 40,
        "closed detail before opens: {bells} {opens}"
    );
    assert!(
        opens == 0 || closes == 40,
        "opens before closes: {opens} {closes}"
    );
    assert_eq!(
        (bells, opens),
        (0, 0),
        "this budget took the detail and the opens"
    );
    assert!(closes > 0, "and kept closes: {closes}");
    assert!(
        new.is_open(run) && new.is_open(open_turn),
        "the open path crosses"
    );
    let orphaned = new
        .window()
        .filter(|r| r.op == aterm_slog::Op::Close && r.k == Kind::Turn)
        .map(|r| r.node)
        .find(|t| {
            !new.window()
                .any(|r| r.node == *t && r.op == aterm_slog::Op::Open)
        })
        .expect("a close whose open was shed");
    assert_eq!(
        new.parent_of(orphaned),
        Some(run),
        "{orphaned}'s parent crossed"
    );
    let roll = new.open_entry(run).expect("open").roll;
    assert_eq!(
        u64::from(roll.lost),
        cut.shed,
        "counted into the nearest open node"
    );
}

#[test]
fn a_carry_whose_open_path_passes_32_kib_is_not_carried() {
    let mut small = Log::new(1);
    agent_session(&mut small, 0);
    let all = small.carry(CarryBudget::SESSION, &[]).expect("carried");
    let boundary = CarryBudget {
        total: all.skeleton_bytes,
        skeleton: all.skeleton_bytes,
        records: small.len(),
    };
    let exact = small.carry(boundary, &[]).expect("the mandatory part fits");
    assert!(exact.bytes <= boundary.total);
    assert_eq!(exact.carried as usize, boundary.records);
    for refused in [
        CarryBudget {
            total: 0,
            ..boundary
        },
        CarryBudget {
            total: boundary.total - 1,
            ..boundary
        },
        CarryBudget {
            records: 0,
            ..boundary
        },
        CarryBudget {
            records: boundary.records - 1,
            ..boundary
        },
        CarryBudget {
            skeleton: boundary.skeleton - 1,
            ..boundary
        },
    ] {
        assert!(
            small.carry(refused, &[]).is_none(),
            "mandatory part exceeds {refused:?}"
        );
    }

    let mut log = Log::new(1);
    let mut at_node = agent_session(&mut log, 0);
    let fat = "x".repeat(40);
    let mut opened = 0;
    while opened < 60 {
        let f = Fields::new()
            .tok(Key::Prog, &fat)
            .and_then(|f| f.tok(Key::Detail, &fat))
            .and_then(|f| f.text(Key::Cwd, &"/very/long/path/".repeat(15)))
            .expect("fat fields");
        at_node = log
            .open(
                Open::new(Kind::Prog, Src::Status, Conf::Strong)
                    .under(at_node)
                    .fields(f),
                at(1 + opened),
            )
            .expect("nested");
        opened += 1;
    }
    assert!(log.pinned_bytes() > 20 * 1024);
    assert_eq!(log.carry(CarryBudget::SESSION, &[]), None);
    let roomy = CarryBudget {
        skeleton: 128 * 1024,
        ..CarryBudget::SESSION
    };
    assert!(
        log.carry(roomy, &[]).is_some(),
        "CONTROL: within a wider skeleton it is"
    );
}

#[test]
fn a_carried_open_node_closes_dur_wall() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1_000),
        )
        .expect("turn");
    let out = log.carry(CarryBudget::SESSION, &[]).expect("carried");
    let a = CarryAdopt {
        at: Stamp::new(WALL0 + 31_000, 200),
        ..adopt_at(&log, 0)
    };
    let (mut new, _, _) = Log::from_carry(&out.lines, a).expect("reads");
    let seq = new
        .close(
            turn,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            Stamp::new(WALL0 + 41_000, 10_200),
        )
        .expect("closes");
    let line = encode(
        new.window().find(|r| r.seq == seq).expect("its close"),
        Tier::Host,
    )
    .expect("fits");
    assert!(line.contains(" dur_ms=40000 dur=wall "), "{line}");
}

#[test]
fn a_tracker_carried_mid_sequence_writes_what_an_uncarried_one_would() {
    let script: Vec<(Verdict, u64)> = vec![
        (Verdict::Busy, 100),
        (Verdict::Prompt(BoxWord::from_detail("edit")), 400),
        (Verdict::Busy, 900),
        (Verdict::Idle, 1500),
        (Verdict::Busy, 1800),
        (Verdict::Idle, 2600),
        (Verdict::Busy, 5000),
        (Verdict::Idle, 5100),
    ];
    let shape = |log: &Log| -> Vec<String> {
        log.window()
            .filter(|r| r.k != Kind::Handoff)
            .map(|r| {
                let mut s = format!("{} {} {}", r.op.word(), r.node, r.k.word());
                if let Some(o) = r.out() {
                    s.push_str(&format!(" out={o}"));
                }
                s
            })
            .collect()
    };
    let run_all = |cut: Option<usize>| -> Vec<String> {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let mut tr = TurnTracker::new(run);
        let mut mono_shift = 0i64;
        for (i, (v, ms)) in script.iter().enumerate() {
            if cut == Some(i) {
                let line = tr.carry_line(at(*ms - 1));
                let out = log.carry(CarryBudget::SESSION, &[line]).expect("carried");
                let now = Stamp::new(WALL0 + *ms - 1, 50);
                mono_shift = 50 - (*ms as i64 - 1);
                let a = CarryAdopt {
                    ep: 2,
                    next_seq: log.last_seq() + 100,
                    ids: log.id_counters(),
                    limits: Limits::SESSION,
                    at: now,
                };
                let (new, host, _) = Log::from_carry(&out.lines, a).expect("reads");
                log = new;
                tr = TurnTracker::from_carry_line(&host[0], now).expect("the tracker reads");
            }
            let mono = (*ms as i64 + mono_shift) as u64;
            let st = Stamp::new(WALL0 + ms, mono);
            tr.observe(&mut log, *v, None, st);
            tr.tick(&mut log, st);
        }
        let end = Stamp::new(WALL0 + 9000, (9000 + mono_shift) as u64);
        tr.tick(&mut log, end);
        shape(&log)
    };
    let plain = run_all(None);
    assert_eq!(
        plain.iter().filter(|l| l.starts_with("close t")).count(),
        2,
        "two turns, the mid-answer idle absorbed: {plain:?}"
    );
    for cut in 1..script.len() {
        assert_eq!(run_all(Some(cut)), plain, "carried before step {cut}");
    }

    for (age, successor_mono) in [(500u64, 0), (2500, 0), (2500, 5000)] {
        let mut plain_log = Log::new(1);
        let run = agent_session(&mut plain_log, 0);
        let mut plain = TurnTracker::new(run);
        plain.observe(&mut plain_log, Verdict::Busy, None, at(100));
        plain.observe(&mut plain_log, Verdict::Idle, None, at(200));
        let source_now = 200 + age;
        let now = Stamp::new(WALL0 + source_now, successor_mono);
        let carry = plain_log
            .carry(CarryBudget::SESSION, &[plain.carry_line(at(source_now))])
            .expect("carried");
        let (mut carried_log, host, _) = Log::from_carry(
            &carry.lines,
            CarryAdopt {
                at: now,
                ..adopt_at(&plain_log, source_now)
            },
        )
        .expect("reads");
        let mut carried = TurnTracker::from_carry_line(&host[0], now).expect("tracker reads");
        assert_eq!(
            carried.view(successor_mono).settle,
            plain.view(source_now).settle
        );
        let wait = aterm_slog::SETTLE_MS.saturating_sub(age);
        if wait > 0 {
            carried.tick(
                &mut carried_log,
                Stamp::new(now.wall_ms + wait - 1, successor_mono + wait - 1),
            );
            assert!(
                carried.view(successor_mono + wait - 1).turn.is_some(),
                "not due yet"
            );
        }
        carried.tick(
            &mut carried_log,
            Stamp::new(now.wall_ms + wait, successor_mono + wait),
        );
        plain.tick(&mut plain_log, at(source_now + wait));
        assert!(
            carried.view(successor_mono + wait).turn.is_none(),
            "due at the same wall instant"
        );
        assert_eq!(
            carried.view(successor_mono + wait).last_close,
            plain.view(source_now + wait).last_close
        );
    }

    let mut plain_log = Log::new(1);
    let run = agent_session(&mut plain_log, 0);
    let mut plain = TurnTracker::new(run);
    plain.observe(&mut plain_log, Verdict::Busy, None, at(9900));
    plain.observe(&mut plain_log, Verdict::Idle, None, at(10_000));
    let now = Stamp::new(WALL0 + 9000, 0);
    let out = plain_log
        .carry(CarryBudget::SESSION, &[plain.carry_line(at(10_000))])
        .expect("carried");
    let (mut carried_log, host, _) = Log::from_carry(
        &out.lines,
        CarryAdopt {
            at: now,
            ..adopt_at(&plain_log, 10_000)
        },
    )
    .expect("reads");
    let mut carried = TurnTracker::from_carry_line(&host[0], now).expect("tracker reads");
    assert_eq!(carried.view(1000).settle, plain.view(10_000).settle);
    carried.tick(&mut carried_log, Stamp::new(WALL0 + 10_999, 1999));
    assert!(
        carried.view(1999).turn.is_some(),
        "not due before its origin plus settle"
    );
    carried.tick(&mut carried_log, Stamp::new(WALL0 + 11_000, 2000));
    plain.tick(&mut plain_log, at(11_000));
    assert_eq!(carried.view(2000).last_close, plain.view(11_000).last_close);
}

#[test]
fn a_carried_input_attributes_only_while_recent_by_the_wall() {
    let opened_by = |input_ms_ago: u64| -> Actor {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let mut tr = TurnTracker::new(run);
        let now_ms = 10_000_000;
        tr.note_input(Actor::Human, at(now_ms - input_ms_ago));
        let line = tr.carry_line(at(now_ms));
        let succ_now = Stamp::new(WALL0 + now_ms + 300, 300);
        let mut tr = TurnTracker::from_carry_line(&line, succ_now).expect("reads");
        tr.observe(&mut log, Verdict::Busy, None, succ_now);
        log.window()
            .find(|r| r.k == Kind::Turn && r.op == aterm_slog::Op::Open)
            .expect("a turn opened")
            .by
    };
    assert_eq!(
        opened_by(3_600_000),
        Actor::Nobody,
        "an hour-old input attributes nothing"
    );
    assert_eq!(
        opened_by(2_000),
        Actor::Human,
        "NEGATIVE CONTROL: 2 s old, it does"
    );
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let line = format!(
        "tracker run={run} turn=none word=unknown interrupted=0 input_by=human input_wall={}",
        WALL0
    );
    let succ_now = Stamp::new(WALL0 + 3_600_000, 300);
    let mut tr = TurnTracker::from_carry_line(&line, succ_now).expect("reads");
    tr.observe(&mut log, Verdict::Busy, None, succ_now);
    assert_eq!(
        log.window().find(|r| r.k == Kind::Turn).map(|r| r.by),
        Some(Actor::Nobody)
    );
}

#[test]
fn a_fenced_log_shows_readers_nothing_past_its_carry() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(10),
        )
        .expect("turn");
    let fence = log.last_seq();
    log.fence_reads();
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        at(20),
    )
    .expect("closes in the overlap");
    let read = log.read(Cursor::new(1, 0), usize::MAX);
    assert!(
        read.recs.iter().all(|r| r.seq <= fence),
        "nothing past the fence"
    );
    assert_eq!(read.next.seq, fence);
    assert!(
        log.reader_state().turn().is_some(),
        "the turn as it stood: open"
    );
    assert!(log.state().turn().is_none(), "the log itself went on");
    assert_eq!(
        log.read(Cursor::new(1, fence + 1), 10).gap.map(|g| g.why),
        Some(GapWhy::Reset),
        "a cursor past the fence is none of this log's yet"
    );
    log.lift_read_fence();
    assert_eq!(
        log.read(Cursor::new(1, fence), 10).recs.len(),
        1,
        "lifted: on"
    );
}

#[test]
fn a_hostile_carry_never_panics_and_is_refused_whole() {
    let mut rng = XorShift::new(17);
    let mut log = Log::new(2);
    agent_session(&mut log, 0);
    traffic(&mut log, &mut rng, 300, 10);
    let good = log
        .carry(CarryBudget::SESSION, &["h v=1".into()])
        .expect("carried");
    let a = adopt_at(&log, 50_000);
    assert!(Log::from_carry(&good.lines, a).is_ok());
    let check = |lines: &[String]| {
        if let Ok((l, _, _)) = Log::from_carry(lines, a) {
            let seqs: Vec<u64> = l.window().map(|r| r.seq).collect();
            assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order");
            for e in l.open_nodes() {
                assert!(
                    l.window()
                        .any(|r| r.node == e.node && r.op == aterm_slog::Op::Open)
                );
            }
        }
    };
    for round in 0..400 {
        let mut lines = good.lines.clone();
        let i = rng.index(lines.len());
        match round % 8 {
            0 => {
                let mut b = lines[i].clone().into_bytes();
                let j = rng.index(b.len().max(1));
                if let Some(x) = b.get_mut(j) {
                    *x ^= 1 << rng.below(7);
                }
                lines[i] = String::from_utf8_lossy(&b).into_owned();
            }
            1 => lines.truncate(i),
            2 => {
                let j = rng.index(lines.len());
                lines.swap(i, j);
            }
            3 => lines[i] = lines[i].replacen(char::is_numeric, "99999999999999999999", 1),
            4 => lines.insert(i, lines[rng.index(lines.len())].clone()),
            5 => lines[i] = format!("{} junk=1", lines[i]),
            6 => lines[i] = lines[i].replace(" r", " p"),
            _ => {
                lines.remove(i);
            }
        }
        check(&lines);
    }
    let refused = |lines: Vec<String>, why: &str| {
        assert!(Log::from_carry(&lines, a).is_err(), "{why} was read");
    };
    let mut v = good.lines.clone();
    v[0] = v[0].replace("v=1", "v=2");
    refused(v, "another version");
    let mut v = good.lines.clone();
    let recs: Vec<usize> = (0..v.len()).filter(|i| v[*i].starts_with("r ")).collect();
    v.swap(recs[3], recs[7]);
    refused(v, "records out of order");
    let mut v = good.lines.clone();
    let open_at = v
        .iter()
        .position(|l| l.starts_with("open "))
        .expect("an open line");
    v[open_at] = v[open_at].replacen("open r", "open r9999", 1);
    refused(v, "a node past the counters");
    let lines_long = |n: usize| {
        let mut v = good.lines.clone();
        v.resize(n, String::new());
        matches!(
            Log::from_carry(&v, a),
            Err(aterm_slog::CarryError::Bound(_))
        )
    };
    assert!(
        lines_long(aterm_slog::CARRY_MAX_LINES + 1),
        "more lines than a carry holds"
    );
    assert!(!lines_long(aterm_slog::CARRY_MAX_LINES));
    let mut v = good.lines.clone();
    v[recs[0]] = format!("r {}", "x".repeat(2000));
    refused(v, "a line past its bound");
    let mut v = good.lines.clone();
    v.retain(|l| !(l.starts_with("r ") && l.contains(" open r1 ")));
    refused(v, "an open node without its open record");

    let mut v = good.lines.clone();
    let close = Rec {
        seq: log.last_seq() + 1,
        t: WALL0 + 49_000,
        op: aterm_slog::Op::Close,
        node: Node::SESSION,
        par: None,
        k: Kind::Spawn,
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Human,
        f: Fields::new().word(Key::Out, "ui-close").expect("outcome"),
    };
    v[0] = v[0]
        .replace(
            &format!("last={} ", log.last_seq()),
            &format!("last={} ", close.seq),
        )
        .replace(
            &format!("carried={} ", good.carried),
            &format!("carried={} ", good.carried + 1),
        )
        .replace(
            &format!("lines={}", good.lines.len()),
            &format!("lines={}", good.lines.len() + 1),
        );
    v.push(format!(
        "r {}",
        encode(&close, Tier::Host).expect("close fits")
    ));
    refused(v, "a carried session close despite an open flag");
    let mut v = good.lines.clone();
    v[0] = v[0].replace(
        &format!("lines={}", good.lines.len()),
        &format!("lines={}", good.lines.len() + 1),
    );
    v.push("fold closed - spawn out=ui-close dur_ms=1 t=1 by=human opened_by=aterm src=aterm conf=exact".into());
    refused(v, "a carried closed fold despite an open flag");

    let mut big = Log::new(2);
    agent_session(&mut big, 0);
    let fat = Fields::new()
        .text(Key::Cwd, &"/w".repeat(128))
        .and_then(|f| f.tok(Key::Detail, &"d".repeat(48)))
        .and_then(|f| f.tok(Key::Prog, &"p".repeat(48)))
        .expect("fat fields");
    for ms in 1..=200 {
        let c = big
            .open(
                Open::new(Kind::Cmd, Src::Shell, Conf::Strong).fields(fat.clone()),
                at(ms),
            )
            .expect("a command");
        big.close(
            c,
            Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact).fields(fat.clone()),
            at(ms),
        )
        .expect("it closes");
    }
    let wrote = |total: usize| {
        big.carry(
            CarryBudget {
                total,
                ..CarryBudget::SESSION
            },
            &["h v=1 pad=".into()],
        )
        .expect("carried")
    };
    let a = adopt_at(&big, 900_000);
    let over = wrote(CarryBudget::SESSION.total + 1024);
    assert!(
        over.bytes > CarryBudget::SESSION.total,
        "PRECONDITION: past a session's budget"
    );
    assert!(Log::from_carry(&over.lines, a).is_ok());
    let mut v = wrote(aterm_slog::CARRY_MAX_BYTES).lines;
    let host = v
        .iter()
        .position(|l| l.starts_with("host "))
        .expect("the host's line");
    let bytes = |v: &[String]| v.iter().map(|l| l.len() + 1).sum::<usize>();
    let pad = aterm_slog::CARRY_MAX_BYTES - bytes(&v);
    v[host].push_str(&"x".repeat(pad));
    assert!(Log::from_carry(&v, a).is_ok(), "a carry at the bound reads");
    v[host].push('x');
    assert!(matches!(
        Log::from_carry(&v, a),
        Err(aterm_slog::CarryError::Bound(_))
    ));
}

#[test]
fn a_carried_log_takes_the_manifests_reserved_ids() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let out = log.carry(CarryBudget::SESSION, &[]).expect("carried");
    let reserved = IdCounters {
        run: 65_535,
        turn: 4096,
        step: 4096,
    };
    let (mut new, _, _) = Log::from_carry(
        &out.lines,
        CarryAdopt {
            ids: reserved,
            ..adopt_at(&log, 1_000)
        },
    )
    .expect("reads");
    let n = new
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(2_000))
        .expect("a run");
    assert_eq!(n.n, 65_536);
}

#[test]
fn a_carry_past_the_seq_limit_or_the_manifests_count_is_refused() {
    let mut rng = XorShift::new(23);
    let mut log = Log::new(2);
    agent_session(&mut log, 0);
    traffic(&mut log, &mut rng, 60, 10);
    let good = log.carry(CarryBudget::SESSION, &[]).expect("carried");
    let a = adopt_at(&log, 50_000);
    let head = |lines: &[String], last: u64| -> Vec<String> {
        let mut v = lines.to_vec();
        v[0] = v[0].replace(
            &format!(" last={} ", log.last_seq()),
            &format!(" last={last} "),
        );
        assert_ne!(v[0], lines[0], "the head was edited");
        v
    };
    for (last, next_seq, why) in [
        (u64::MAX - 2, u64::MAX, "a head near u64::MAX"),
        (
            aterm_slog::SEQ_LIMIT + 1,
            aterm_slog::SEQ_LIMIT + 10,
            "a head past the limit",
        ),
        (log.last_seq(), log.last_seq(), "a head at the count"),
        (log.last_seq(), 1, "no count carried beside a head"),
    ] {
        let lines = if last == log.last_seq() {
            good.lines.clone()
        } else {
            head(&good.lines, last)
        };
        let got = Log::from_carry(&lines, CarryAdopt { next_seq, ..a });
        assert!(got.is_err(), "{why} was read");
    }
    let after = Log::after_handoff(
        3,
        Limits::SESSION,
        log.id_counters(),
        i64::MAX as u64,
        at(60_000),
    );
    assert_eq!(
        (after.first_seq(), after.last_seq()),
        (1, 1),
        "a count no log reaches is none"
    );

    assert!(Log::from_carry(&good.lines, a).is_ok());
    let (tight, _, _) = Log::from_carry(
        &good.lines,
        CarryAdopt {
            next_seq: log.last_seq() + 1,
            ..a
        },
    )
    .expect("a head just below the count reads");
    assert_eq!(
        tight.last_seq(),
        log.last_seq() + 1,
        "the handoff mark next"
    );
}

#[test]
fn a_host_line_past_the_bound_is_left_out_and_the_carry_reads() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    let long = format!("resets {}", "\u{e9}".repeat(600));
    tr.observe(&mut log, Verdict::Busy, None, at(100));
    tr.observe(
        &mut log,
        Verdict::Wall(WallKind::UsageSession),
        Some(&long),
        at(200),
    );
    let tracker = tr.carry_line(at(300));
    assert!(
        "host ".len() + tracker.len() <= aterm_slog::CARRY_MAX_LINE,
        "{} bytes",
        tracker.len()
    );
    let mut host = vec!["state v=1 a=b".to_string(), "x".repeat(2000), tracker];
    host.extend((0..30).map(|i| format!("pending n={i}")));
    let out = log.carry(CarryBudget::SESSION, &host).expect("carried");
    assert!(
        out.lines
            .iter()
            .all(|l| l.len() <= aterm_slog::CARRY_MAX_LINE),
        "every line within the bound"
    );
    let a = adopt_at(&log, 1_000);
    let (_, back, _) = Log::from_carry(&out.lines, a).expect("the carry reads");
    assert_eq!(back.len(), aterm_slog::CARRY_MAX_HOST_LINES);
    assert_eq!(back[0], host[0]);
    assert_eq!(
        back[1], host[2],
        "the tracker crossed, the long line did not"
    );
    assert!(TurnTracker::from_carry_line(&back[1], at(1_000)).is_some());
    let held = log
        .window()
        .rfind(|r| r.k == Kind::Wall && r.op == aterm_slog::Op::Open)
        .and_then(|r| r.f.get_str(Key::Detail).map(str::to_string))
        .expect("the wall's detail field");
    let carried = back[1]
        .split(' ')
        .find_map(|t| t.strip_prefix("step_detail="))
        .map(aterm_codec::pct::decode)
        .expect("the step's detail crossed");
    assert_eq!(carried, held);

    let mut lines = out.lines.clone();
    let i = lines
        .iter()
        .position(|l| l.starts_with("host "))
        .expect("a host line");
    lines[i] = format!("host {}", "x".repeat(2000));
    assert!(Log::from_carry(&lines, a).is_err());
    let refused = |lines: &[String]| {
        matches!(
            Log::from_carry(lines, a),
            Err(aterm_slog::CarryError::Bound(_))
        )
    };
    let mut more = out.lines.clone();
    more.insert(i, "host pending n=99".into());
    more[0] = more[0].replace(
        &format!("lines={}", out.lines.len()),
        &format!("lines={}", out.lines.len() + 1),
    );
    assert!(refused(&more), "a 17th host line");
    let mut wide = out.lines.clone();
    for l in wide.iter_mut().filter(|l| l.starts_with("host ")).take(5) {
        *l = format!("host {}", "x".repeat(900));
    }
    assert!(refused(&wide), "five 900-byte host lines");
    let max = aterm_slog::CARRY_MAX_LINE - "host ".len();
    let whole = (aterm_slog::CARRY_MAX_HOST_BYTES - 1) / (max + 1);
    let mut full = vec!["x".repeat(max); whole];
    full.push("y".repeat(aterm_slog::CARRY_MAX_HOST_BYTES - whole * (max + 1) - 1));
    let offered = [&["w".repeat(max + 1)], full.as_slice(), &["z".to_string()]].concat();
    let at_bounds = log.carry(CarryBudget::SESSION, &offered).expect("carried");
    let (_, back, _) = Log::from_carry(&at_bounds.lines, a).expect("the carry reads");
    assert_eq!(back, full, "every host line at the bounds crossed");
    let first = at_bounds
        .lines
        .iter()
        .position(|l| l.starts_with("host "))
        .expect("a host line");
    let last = first + whole;
    let mut long = at_bounds.lines.clone();
    long[first].push('x');
    long[last].pop();
    assert!(refused(&long), "a line one byte past its bound");
    let mut over = at_bounds.lines.clone();
    over[last].push('y');
    assert!(refused(&over), "host lines one byte past their bytes");
}

fn with_line_at(lines: &[String], i: usize, line: String) -> Vec<String> {
    let mut v = lines.to_vec();
    v.insert(i, line);
    let count = v.len();
    v[0] = v[0]
        .split(' ')
        .map(|w| {
            if w.starts_with("lines=") {
                format!("lines={count}")
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    v
}

fn fold_open_line(lines: &[String], node: Node) -> usize {
    lines
        .iter()
        .position(|l| l.starts_with(&format!("fold open {node} ")))
        .expect("the node's fold open line")
}

fn carried_turn() -> (Log, Node, Node, Node, Vec<String>) {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let ghost = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(5))
        .expect("a cmd");
    log.close(
        ghost,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(6),
    )
    .expect("the cmd closes");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(run)
                .by(Actor::Human),
            at(10),
        )
        .expect("a turn");
    let out = log.carry(CarryBudget::SESSION, &[]).expect("fits");
    (log, run, turn, ghost, out.lines)
}

#[test]
fn a_duplicated_fold_open_line_is_refused() {
    let (log, _, turn, _, lines) = carried_turn();
    let close = |mut back: Log| {
        back.close(
            turn,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(950),
        )
        .expect("the turn closes");
        back.fold().state().open[1].as_ref().map(|o| o.node)
    };
    let (back, _, _) = Log::from_carry(&lines, adopt_at(&log, 900)).expect("CONTROL: reads");
    assert_eq!(
        close(back),
        None,
        "CONTROL: the turn's close closes it in the fold"
    );

    let i = fold_open_line(&lines, turn);
    let dup = with_line_at(&lines, i, lines[i].clone());
    match Log::from_carry(&dup, adopt_at(&log, 900)) {
        Err(e) => assert_eq!(
            e,
            CarryError::Fold(SnapError::Log),
            "refused by the fold's agreement with its log: {}",
            e.why()
        ),
        Ok((back, _, _)) => panic!(
            "a duplicated fold open line was accepted; after the turn's close the fold \
             still shows open turn {:?}",
            close(back)
        ),
    }
}

#[test]
fn a_fold_open_line_for_a_node_the_log_does_not_hold_open_is_refused() {
    let (log, run, turn, ghost, lines) = carried_turn();
    assert!(!log.is_open(ghost), "CONTROL: the log holds the cmd closed");
    let refused = |v: &[String], what: &str| match Log::from_carry(v, adopt_at(&log, 900)) {
        Err(e) => assert_eq!(e, CarryError::Fold(SnapError::Log), "{what}: {}", e.why()),
        Ok((back, _, _)) => panic!(
            "{what} was accepted: the fold's open run {:?}, turn {:?}; the log's deepest \
             open run {:?}",
            back.fold().state().open[0]
                .as_ref()
                .map(|o| (o.node, o.kind)),
            back.fold().state().open[1]
                .as_ref()
                .map(|o| (o.node, o.par)),
            back.deepest_open(aterm_slog::Lvl::Run)
        ),
    };

    let i = fold_open_line(&lines, turn);
    let ghost_line = lines[i].replacen(
        &format!("fold open {turn} turn par={run} "),
        &format!("fold open {ghost} cmd par=- "),
        1,
    );
    assert_ne!(ghost_line, lines[i], "CONTROL: the line was rewritten");
    refused(
        &with_line_at(&lines, i, ghost_line),
        "a fold open line for a run the log holds closed",
    );

    let mut moved = lines.clone();
    moved[i] = moved[i].replacen(&format!(" par={run} "), " par=- ", 1);
    assert_ne!(moved[i], lines[i], "CONTROL: the line was rewritten");
    refused(&moved, "the open turn's fold line under the session");

    let r = fold_open_line(&lines, run);
    let mut retyped = lines.clone();
    retyped[r] = retyped[r].replacen(
        &format!("fold open {run} agent "),
        &format!("fold open {run} cmd "),
        1,
    );
    assert_ne!(retyped[r], lines[r], "CONTROL: the line was rewritten");
    refused(&retyped, "the open run's fold line of another run kind");
}

#[test]
fn a_closed_summary_whose_fold_holds_a_node_open_is_refused() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact),
        at(20),
    )
    .expect("the session closes");
    let summary = log.closed_summary();
    assert!(
        Log::from_closed_summary(&summary).is_ok(),
        "CONTROL: the summary reads"
    );
    assert!(!summary.contains("fold open "), "CONTROL: none written");
    let open = format!(
        "fold open {run} agent par=- by=- src=status conf=strong t={}",
        at(0).wall_ms
    );
    let forged = summary.replacen("\nfold totals ", &format!("\n{open}\nfold totals "), 1);
    assert_ne!(forged, summary, "CONTROL: the line was put in");
    match Log::from_closed_summary(&forged) {
        Err(e) => assert_eq!(e, CarryError::Fold(SnapError::Log), "{}", e.why()),
        Ok(back) => panic!(
            "a closed summary holding {run} open was accepted: the fold's open run {:?}",
            back.fold().state().open[0].as_ref().map(|o| o.node)
        ),
    }
}
