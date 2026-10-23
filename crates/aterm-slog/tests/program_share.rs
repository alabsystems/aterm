// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each node's program marks hold at most [`program_share`] of the ring (`PROGRAM_SHARE_MAX`, 64 at a session's bound).

use aterm_slog::{
    Actor, Conf, Fields, Key, Kind, Limits, Log, MAX_RECORDS, Mark, Node, Open, PROGRAM_SHARE_MAX,
    Src, Tier, encode, program_share,
};

use crate::support::at;

fn session_with_run(log: &mut Log) -> Node {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(1),
    )
    .expect("the session opens");
    log.open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(2))
        .expect("a run opens")
}

fn program(log: &mut Log, node: Node, state: &str, ms: u64) -> u64 {
    log.mark(
        Mark::new(Kind::Program, Src::Engine, Conf::Claim)
            .at(node)
            .fields(Fields::new().word(Key::State, state).expect("a state")),
        at(ms),
    )
    .expect("a program mark")
}

fn programs(log: &Log, node: Node) -> Vec<(u64, String)> {
    log.window()
        .filter(|r| r.k == Kind::Program && r.node == node)
        .map(|r| {
            let line = encode(r, Tier::Wire).expect("fits");
            let state = line
                .split(' ')
                .find_map(|w| w.strip_prefix("state="))
                .expect("a state")
                .to_owned();
            (r.seq, state)
        })
        .collect()
}

#[test]
fn the_share_is_64_a_session_ring_and_a_32nd_of_a_smaller_one() {
    assert_eq!(PROGRAM_SHARE_MAX, 64);
    assert_eq!(program_share(Limits::SESSION), 64);
    assert_eq!(Limits::SESSION.records, MAX_RECORDS);
    let of = |records| program_share(Limits::new(records, 1 << 20).expect("limits"));
    assert_eq!(
        (of(64), of(320), of(31), of(1), of(1 << 20)),
        (2, 10, 1, 1, 64)
    );
}

#[test]
fn a_mail_mark_written_before_300_flaps_is_still_in_the_log() {
    let mut log = Log::new(3);
    let run = session_with_run(&mut log);
    let mail = log
        .mark(
            Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
                .at(Node::SESSION)
                .by(Actor::Bridge)
                .fields(
                    Fields::new()
                        .word(Key::Dir, "in")
                        .and_then(|f| f.word(Key::MailKind, "task"))
                        .and_then(|f| f.u(Key::Off, 77))
                        .expect("mail fields"),
                ),
            at(3),
        )
        .expect("the mail mark");
    log.set_byte_budget(16 * 1024);
    let mut one = 0;
    for i in 0..300u64 {
        let state = ["working", "idle", "blocked"][(i % 3) as usize];
        let seq = program(&mut log, run, state, 10 + i);
        if one == 0 {
            one = encode(log.window().last().expect("the mark"), Tier::Content)
                .expect("fits")
                .len();
            assert_eq!(log.last_seq(), seq);
        }
    }
    assert!(
        300 * one > log.budget_bytes(),
        "300 program marks of {one} bytes overflow the budget: the test means something"
    );
    assert!(
        log.window().any(|r| r.seq == mail && r.k == Kind::Mail),
        "the mail mark written before the flaps is in the log"
    );
    let kept = programs(&log, run);
    assert_eq!(kept.len(), PROGRAM_SHARE_MAX, "the run's share");
    assert_eq!(log.programs_held(run), PROGRAM_SHARE_MAX);
    let first = log.last_seq() - PROGRAM_SHARE_MAX as u64 + 1;
    assert_eq!(
        kept.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        (first..=log.last_seq()).collect::<Vec<_>>(),
        "the oldest went first"
    );
    assert_eq!(
        kept.last().map(|(_, s)| s.as_str()),
        Some("blocked"),
        "the 300th flap (299 % 3 = 2)"
    );
    assert_eq!(
        log.lost(),
        300 - PROGRAM_SHARE_MAX as u64,
        "each counted lost"
    );
    assert_eq!(log.len(), 3 + PROGRAM_SHARE_MAX);
}

#[test]
fn the_share_is_per_node_and_takes_neither_the_newest_nor_a_fenced_mark() {
    let mut log = Log::with_limits(0, Limits::new(64, 1 << 20).expect("a small ring"));
    let share = program_share(log.limits());
    assert_eq!(share, 2);
    let run = session_with_run(&mut log);
    let bell = |log: &mut Log, ms| {
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(Node::SESSION),
            at(ms),
        )
        .expect("a bell")
    };
    for i in 0..5 {
        program(&mut log, run, "working", 10 + i);
        program(&mut log, Node::SESSION, "idle", 20 + i);
        bell(&mut log, 30 + i);
    }
    assert_eq!(
        (log.programs_held(run), log.programs_held(Node::SESSION)),
        (2, 2)
    );
    assert_eq!(log.window().filter(|r| r.k == Kind::Bell).count(), 5);
    assert_eq!(log.lost(), 6, "three of each node's five");

    let other = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(40))
        .expect("a second run");
    program(&mut log, other, "working", 41);
    assert_eq!(
        (log.programs_held(run), log.programs_held(other)),
        (2, 1),
        "the first run's marks stay"
    );

    log.fence_reads();
    let fenced: Vec<u64> = programs(&log, run).into_iter().map(|(s, _)| s).collect();
    program(&mut log, run, "blocked", 50);
    let newest = program(&mut log, run, "done", 51);
    let now: Vec<u64> = programs(&log, run).into_iter().map(|(s, _)| s).collect();
    assert_eq!(
        now,
        [fenced.clone(), vec![newest]].concat(),
        "fenced kept, newest kept"
    );
    let lost = log.lost();
    log.lift_read_fence();
    assert_eq!(
        programs(&log, run),
        vec![
            (fenced[1], "working".to_owned()),
            (newest, "done".to_owned())
        ],
        "lifted, the run's oldest went"
    );
    assert_eq!(log.lost(), lost + 1);
    assert_eq!(
        (log.programs_held(Node::SESSION), log.programs_held(other)),
        (2, 1)
    );
    assert_eq!(log.window().filter(|r| r.k == Kind::Bell).count(), 5);
}
